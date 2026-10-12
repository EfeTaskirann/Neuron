//! OTLP shape: round-trip a known span batch through
//! [`crate::telemetry::otlp::build_envelope`] and compare against
//! `fixtures/expected.json`, plus the deterministic-id, default-deny
//! redaction, root-span, in-flight-duration, and truncation edges.

use serde_json::Value;

use crate::telemetry::otlp::{
    build_envelope, span_id_for, trace_id_for, ExportOptions, StoredSpan,
};

/// Convenience: the fixture file pre-dates security-review M1, so
/// every test that compares against it needs prompt/response in the
/// envelope. New tests that exercise the default-deny path use
/// `ExportOptions::default()` directly.
fn opts_with_prompts() -> ExportOptions {
    ExportOptions {
        include_prompt_response: true,
    }
}

const FIXTURE_EXPECTED: &str = include_str!("fixtures/expected.json");

/// Two-span batch matching the JSON fixture. One root span carrying
/// a prompt/response pair plus token attrs, plus one child span with
/// an `error` attribute so the OK→ERROR status branch is exercised.
fn fixture_batch() -> Vec<StoredSpan> {
    vec![
        StoredSpan {
            id: "s-1".into(),
            run_id: "r-fixture".into(),
            parent_span_id: None,
            name: "planner".into(),
            t0_ms: 1000,
            duration_ms: Some(50_000),
            attrs_json: r#"{"node":"planner","tokens_in":120}"#.into(),
            prompt: Some("Plan the day.".into()),
            response: Some("1) coffee 2) ship".into()),
        },
        StoredSpan {
            id: "s-2".into(),
            run_id: "r-fixture".into(),
            parent_span_id: Some("s-1".into()),
            name: "tool.fetch".into(),
            t0_ms: 60_000,
            duration_ms: Some(25_000),
            attrs_json: r#"{"error":"boom"}"#.into(),
            prompt: None,
            response: None,
        },
    ]
}

/// The deterministic-id helpers must hash to a fixed prefix for a
/// given input. If we ever swap the hash function, this test (plus
/// the fixture below) catches the drift before it reaches the
/// collector.
#[test]
fn deterministic_ids_match_documented_lengths() {
    let trace = trace_id_for("r-fixture");
    let span = span_id_for("s-1");
    assert_eq!(trace.len(), 32, "traceId is 16 bytes hex = 32 chars");
    assert_eq!(span.len(), 16, "spanId is 8 bytes hex = 16 chars");
    // SHA-256("r-fixture") = 99ea096ae86ac121... (precomputed).
    assert_eq!(trace, "99ea096ae86ac121d25e036eba0b2a76");
    assert_eq!(span, "6a840baf5d8c3ff2");
}

/// Round-trip: build an envelope, serialize → parse it back as a
/// `Value`, and compare against the checked-in fixture. Comparing
/// post-parse means whitespace / key-order differences in the
/// fixture file don't break the test.
#[test]
fn envelope_matches_fixture() {
    let (got, _ids) = build_envelope(&fixture_batch(), opts_with_prompts())
        .expect("envelope");
    let want: Value =
        serde_json::from_str(FIXTURE_EXPECTED).expect("fixture parses");
    assert_eq!(
        got, want,
        "envelope drift — fixture: {FIXTURE_EXPECTED}\nactual: {got:#}"
    );
}

/// Security-review M1: prompt/response are off by default. The
/// envelope still ships every other attribute for the same batch —
/// only the `gen_ai.prompt` and `gen_ai.completion` keys disappear.
#[test]
fn default_options_redact_prompt_and_response() {
    let (got, _ids) = build_envelope(&fixture_batch(), ExportOptions::default())
        .expect("envelope");
    let attrs = got["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"]
        .as_array()
        .expect("attrs array")
        .clone();
    let keys: Vec<&str> = attrs
        .iter()
        .map(|a| a["key"].as_str().unwrap_or(""))
        .collect();
    assert!(
        keys.contains(&"node"),
        "non-secret attrs must still be present: {keys:?}"
    );
    assert!(
        keys.contains(&"tokens_in"),
        "non-secret attrs must still be present: {keys:?}"
    );
    assert!(
        !keys.contains(&"gen_ai.prompt"),
        "default-deny: prompt must be omitted unless opted in"
    );
    assert!(
        !keys.contains(&"gen_ai.completion"),
        "default-deny: response must be omitted unless opted in"
    );
}

/// `parentSpanId` is omitted (not null) on root spans. The proto
/// allows either, but most collectors prefer the field absent so
/// the trace-tree builder doesn't try to chase a null parent.
#[test]
fn root_span_omits_parent_span_id() {
    let batch = vec![StoredSpan {
        id: "root".into(),
        run_id: "r".into(),
        parent_span_id: None,
        name: "n".into(),
        t0_ms: 0,
        duration_ms: Some(1),
        attrs_json: "{}".into(),
        prompt: None,
        response: None,
    }];
    let (env, _) = build_envelope(&batch, opts_with_prompts()).unwrap();
    let span = &env["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert!(
        span.get("parentSpanId").is_none(),
        "root span must not carry parentSpanId"
    );
}

/// In-flight spans (`duration_ms IS NULL`) DO get serialized — the
/// SQL filter at the export sweep step is what excludes them from
/// the wire. Translating one explicitly here covers the path where
/// a future caller wires the serializer to a different source.
#[test]
fn span_without_duration_omits_end_time() {
    let batch = vec![StoredSpan {
        id: "in-flight".into(),
        run_id: "r".into(),
        parent_span_id: None,
        name: "n".into(),
        t0_ms: 100,
        duration_ms: None,
        attrs_json: "{}".into(),
        prompt: None,
        response: None,
    }];
    let (env, _) = build_envelope(&batch, opts_with_prompts()).unwrap();
    let span = &env["resourceSpans"][0]["scopeSpans"][0]["spans"][0];
    assert!(span.get("endTimeUnixNano").is_none());
    assert_eq!(span["startTimeUnixNano"], "100000000");
}

/// `prompt`/`response` longer than 1 KiB is truncated.
#[test]
fn long_prompt_is_truncated_to_1kib() {
    let big = "x".repeat(4096);
    let batch = vec![StoredSpan {
        id: "s".into(),
        run_id: "r".into(),
        parent_span_id: None,
        name: "n".into(),
        t0_ms: 0,
        duration_ms: Some(1),
        attrs_json: "{}".into(),
        prompt: Some(big.clone()),
        response: None,
    }];
    let (env, _) = build_envelope(&batch, opts_with_prompts()).unwrap();
    let attrs = &env["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
    let prompt_attr = attrs
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["key"] == "gen_ai.prompt")
        .expect("prompt attr present");
    let value = prompt_attr["value"]["stringValue"].as_str().unwrap();
    assert_eq!(
        value.chars().count(),
        crate::telemetry::otlp::ATTR_TEXT_CAP,
        "expected exactly 1 KiB of chars after truncation"
    );
}

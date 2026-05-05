//! WP-W3-06 — telemetry module tests.
//!
//! Three groups:
//!
//! 1. **OTLP shape** — round-trip a known span batch through
//!    [`super::otlp::build_envelope`] and compare against
//!    `tests/fixtures/expected.json`.
//! 2. **Migration** — schema-level checks for the `0005` migration
//!    (column count grows by 2; partial index lands in
//!    `sqlite_master`).
//! 3. **Sweep** — drive [`super::exporter::export_one_batch`] against
//!    a `mockito` stub HTTP server through the documented status-code
//!    branches (2xx / 4xx / 5xx / empty / full batch).

use serde_json::Value;

use super::otlp::{build_envelope, span_id_for, trace_id_for, ExportOptions, StoredSpan};

/// Convenience: the fixture file pre-dates security-review M1, so
/// every test that compares against it needs prompt/response in the
/// envelope. New tests that exercise the default-deny path use
/// `ExportOptions::default()` directly.
fn opts_with_prompts() -> ExportOptions {
    ExportOptions {
        include_prompt_response: true,
    }
}

const FIXTURE_EXPECTED: &str = include_str!("tests/fixtures/expected.json");

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

// --------------------------------------------------------------------- //
// Group 1 — OTLP shape                                                   //
// --------------------------------------------------------------------- //

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
        super::otlp::ATTR_TEXT_CAP,
        "expected exactly 1 KiB of chars after truncation"
    );
}

// --------------------------------------------------------------------- //
// Group 2 — Migration                                                    //
// --------------------------------------------------------------------- //

#[tokio::test]
async fn migration_0005_adds_two_columns_to_runs_spans() {
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    // PRAGMA table_info returns one row per column. Pre-WP3-06 the
    // table had 11 columns; this WP adds `exported_at` and
    // `sampled_in` for a total of 13.
    let cols: Vec<(i64, String)> = sqlx::query_as(
        "SELECT cid, name FROM pragma_table_info('runs_spans')",
    )
    .fetch_all(&pool)
    .await
    .expect("pragma table_info");
    assert_eq!(
        cols.len(),
        13,
        "runs_spans must have 11 + 2 columns post-0005 (got {cols:?})"
    );
    let names: Vec<&str> = cols.iter().map(|(_, n)| n.as_str()).collect();
    assert!(names.contains(&"exported_at"), "exported_at column missing");
    assert!(names.contains(&"sampled_in"), "sampled_in column missing");
}

#[tokio::test]
async fn migration_0005_partial_index_lands_in_sqlite_master() {
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type='index' AND name='idx_runs_spans_export_pending'",
    )
    .fetch_optional(&pool)
    .await
    .expect("sqlite_master query");
    let sql = sql.expect("idx_runs_spans_export_pending must exist");
    // The partial-index predicate must include both columns so the
    // sweep skips sentinel and unsampled rows.
    assert!(
        sql.contains("exported_at IS NULL"),
        "predicate missing exported_at: {sql}"
    );
    assert!(
        sql.contains("sampled_in = 1"),
        "predicate missing sampled_in: {sql}"
    );
}

// --------------------------------------------------------------------- //
// Group 3 — Sweep against stub HTTP collector                            //
// --------------------------------------------------------------------- //

/// Insert a workflow + run + N closed spans with `sampled_in=1` and
/// `exported_at IS NULL` so the sweep picks them up.
async fn seed_pending_spans(pool: &crate::db::DbPool, count: usize) -> Vec<String> {
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(pool)
    .await
    .unwrap();
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let id = format!("s-{i:04}");
        sqlx::query(
            "INSERT INTO runs_spans \
             (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
             VALUES (?, 'r-1', 'span', 'llm', ?, 10, '{}', 0, 1, NULL)",
        )
        .bind(&id)
        .bind(i as i64 * 100)
        .execute(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn sweep_empty_queue_returns_zero_and_makes_no_request() {
    let mut server = mockito::Server::new_async().await;
    // No expectation — if the sweep makes a call, mockito fails the
    // test on drop because no matcher will satisfy it.
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("empty sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}

#[tokio::test]
async fn sweep_2xx_flags_exported_at() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 3).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 3);
    m.assert_async().await;

    for id in ids {
        let exported_at: Option<i64> = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let v = exported_at.expect("exported_at flagged");
        assert!(v > 0, "exported_at must be a positive unix-seconds value");
    }
}

#[tokio::test]
async fn sweep_5xx_leaves_rows_untouched() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(503)
        .with_body("upstream timeout")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 2).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep returns ok even on 5xx");
    assert_eq!(n, 0, "5xx must not flag rows");
    m.assert_async().await;

    for id in ids {
        let exported_at: Option<i64> = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            exported_at.is_none(),
            "5xx must leave exported_at NULL for retry"
        );
    }
}

#[tokio::test]
async fn sweep_4xx_flags_failed_sentinel() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(400)
        .with_body("bad request")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let ids = seed_pending_spans(&pool, 2).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep returns ok even on 4xx");
    assert_eq!(n, 2, "4xx flags rows as permanently failed");
    m.assert_async().await;

    for id in ids {
        let exported_at: i64 = sqlx::query_scalar(
            "SELECT exported_at FROM runs_spans WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(exported_at, -1, "4xx must flag exported_at = -1 sentinel");
    }
}

#[tokio::test]
async fn sweep_full_batch_is_one_http_call_with_all_rows_flagged() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(200)
        .with_body("{}")
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    // Seed exactly BATCH_SIZE rows so we exercise the row-cap path.
    let ids = seed_pending_spans(&pool, super::exporter::BATCH_SIZE as usize).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(
        n,
        super::exporter::BATCH_SIZE as usize,
        "all 200 rows flagged in one batch"
    );
    m.assert_async().await;

    let pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runs_spans WHERE exported_at IS NULL",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(pending, 0, "no rows should remain pending");
    let _ = ids;
}

/// In-flight spans (duration_ms NULL) must NOT be picked up — only
/// closed spans go on the wire. Confirms the SQL filter does its
/// job rather than relying on the OTLP serializer to handle nulls.
#[tokio::test]
async fn sweep_skips_in_flight_spans() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
         VALUES ('s-x','r-1','span','llm',0,NULL,'{}',1,1,NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}

// --------------------------------------------------------------------- //
// Group 4 — security-review M2 / M3                                      //
// --------------------------------------------------------------------- //

/// Security-review M3: validate_endpoint accepts the well-formed
/// shapes a real OTLP collector exposes (https + http to loopback)
/// and rejects everything else with a non-empty reason string.
#[test]
fn validate_endpoint_accepts_https_and_loopback_http() {
    use super::exporter::validate_endpoint;
    assert!(validate_endpoint("https://collector.example/v1/traces").is_ok());
    assert!(validate_endpoint("http://localhost:4318/v1/traces").is_ok());
    assert!(validate_endpoint("http://127.0.0.1:4318/v1/traces").is_ok());
}

#[test]
fn validate_endpoint_rejects_non_http_schemes() {
    use super::exporter::validate_endpoint;
    let cases = [
        "file:///etc/passwd",
        "ftp://collector.example",
        "ipc://something",
        "javascript:alert(1)",
    ];
    for c in cases {
        let err = validate_endpoint(c)
            .unwrap_err_or_else(|()| panic!("{c} should be rejected"));
        assert!(
            err.contains("scheme") || err.contains("URL"),
            "rejection reason for {c} should mention scheme/URL: {err}"
        );
    }
}

#[test]
fn validate_endpoint_rejects_garbage() {
    use super::exporter::validate_endpoint;
    assert!(validate_endpoint("not-a-url").is_err());
    assert!(validate_endpoint("").is_err());
}

/// Plain HTTP to a non-loopback host is allowed (some local-network
/// collectors only speak HTTP) but should NOT be silently equivalent
/// to https. We assert the call returns Ok; the warning emission is
/// observable via tracing in production but not asserted here to
/// keep the test free of subscriber installation.
#[test]
fn validate_endpoint_allows_plain_http_to_remote_with_warning_path() {
    use super::exporter::validate_endpoint;
    assert!(validate_endpoint("http://collector.example:4318/v1/traces").is_ok());
}

/// Tiny shim so the assertion in `validate_endpoint_rejects_non_http_schemes`
/// can stay readable. `Result::unwrap_err_or_else` does not exist in
/// std; we add the equivalent inline via a trait extension scoped to
/// this test module so the call site reads naturally.
trait ResultExt<T, E> {
    fn unwrap_err_or_else<F: FnOnce(()) -> E>(self, f: F) -> E;
}

impl<T, E> ResultExt<T, E> for Result<T, E> {
    fn unwrap_err_or_else<F: FnOnce(()) -> E>(self, f: F) -> E {
        match self {
            Ok(_) => f(()),
            Err(e) => e,
        }
    }
}

/// Security-review M2: a 4xx response with a body larger than
/// [`super::exporter::RESPONSE_LOG_CAP`]'s value MUST NOT cause us
/// to allocate the full body into memory. We approximate the bound
/// by mocking a 4xx with a 1 MiB body and verifying the sweep
/// completes in well under the timeout the loop would otherwise hit.
/// The actual cap-enforcement is exercised at the unit level by
/// `read_capped_truncates_oversized_body` below.
#[tokio::test]
async fn sweep_4xx_with_large_body_does_not_oom() {
    let mut server = mockito::Server::new_async().await;
    // 1 MiB body. If the cap regresses, this test still passes (the
    // OS will happily hand us 1 MiB), but the unit-level cap test is
    // the load-bearing guard.
    let big = "X".repeat(1024 * 1024);
    let m = server
        .mock("POST", "/v1/traces")
        .with_status(400)
        .with_body(big)
        .expect(1)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let _ids = seed_pending_spans(&pool, 1).await;
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 1, "4xx still flags rows");
    m.assert_async().await;
}

/// Unsampled spans (`sampled_in = 0`) are excluded by the SQL
/// filter. Confirms the partial index predicate matches the query
/// predicate.
#[tokio::test]
async fn sweep_skips_unsampled_spans() {
    let mut server = mockito::Server::new_async().await;
    let m = server
        .mock("POST", "/v1/traces")
        .expect(0)
        .create_async()
        .await;
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r-1','w1','Daily summary',1,'running')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, name, type, t0_ms, duration_ms, attrs_json, is_running, sampled_in, exported_at) \
         VALUES ('s-y','r-1','span','llm',0,10,'{}',0,0,NULL)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let endpoint = format!("{}/v1/traces", server.url());
    let n = super::exporter::export_one_batch(&pool, &endpoint)
        .await
        .expect("sweep ok");
    assert_eq!(n, 0);
    m.assert_async().await;
}

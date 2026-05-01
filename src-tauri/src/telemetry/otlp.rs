//! WP-W3-06 — manual OTLP/HTTP/JSON v1.3 serializer.
//!
//! Why hand-rolled? The `opentelemetry` crate family is sprawling
//! (sdk + sdk-trace + otlp + reqwest-blocking transport, plus their
//! transitive deps), and the wire shape we need is small enough that
//! pulling that tree just to hand the same JSON to `reqwest` is bad
//! value. The serializer here is ~150 LOC and produces an envelope
//! the OTLP/HTTP spec validates byte-for-byte (the round-trip test
//! in `tests.rs` checks against a checked-in fixture).
//!
//! v1.3 envelope shape:
//!
//! ```jsonc
//! {
//!   "resourceSpans": [{
//!     "resource":   { "attributes": [{key, value:{stringValue}}, ...] },
//!     "scopeSpans": [{
//!       "scope": { "name": "neuron-agent-runtime" },
//!       "spans": [{
//!         "traceId":..., "spanId":..., "parentSpanId":...,
//!         "name":..., "kind": 1,
//!         "startTimeUnixNano": "...", "endTimeUnixNano": "...",
//!         "attributes": [...],
//!         "status": { "code": 1 }
//!       }]
//!     }]
//!   }]
//! }
//! ```

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::AppError;

/// Service name advertised on every export. The OTLP collector
/// indexes on this so all Neuron-emitted spans land in the same
/// service bucket regardless of run/host. Matches the `service.name`
/// attribute semantics from semconv 1.x.
const SERVICE_NAME: &str = "neuron";

/// Scope name on every emitted scope. Identifies the producing
/// instrumentation library so collectors can split spans by source.
const SCOPE_NAME: &str = "neuron-agent-runtime";

/// SPAN_KIND_INTERNAL — Neuron's spans are LLM/tool/logic nodes,
/// none of which fit OTLP's CLIENT/SERVER/PRODUCER/CONSUMER buckets.
/// `1` is the canonical INTERNAL value.
const KIND_INTERNAL: i32 = 1;

/// `Status.code` enum: STATUS_CODE_OK. The proto allows OK to be
/// implicit (omit the field), but emitting it explicitly costs a
/// few bytes and removes ambiguity for collectors that strict-parse.
const STATUS_OK: i32 = 1;

/// `Status.code` enum: STATUS_CODE_ERROR.
const STATUS_ERROR: i32 = 2;

/// Truncation cap for `prompt`/`response` fields shipped as
/// `gen_ai.prompt`/`gen_ai.completion` attributes. 1 KiB matches
/// the WP scope ("truncated to 1 KiB"). Keeps the envelope from
/// exploding when a model emits a multi-MB response while still
/// surfacing enough context for trace inspection.
pub const ATTR_TEXT_CAP: usize = 1024;

/// Hex length of an OTLP traceId — 16 bytes → 32 hex chars. Const
/// so the deterministic-id helpers can't drift away from the spec.
const TRACE_ID_LEN: usize = 16;

/// Hex length of an OTLP spanId — 8 bytes → 16 hex chars.
const SPAN_ID_LEN: usize = 8;

/// One row of `runs_spans`, exactly the columns the export sweep
/// fetches. Kept separate from `WireSpan` (sidecar) and `Span`
/// (frontend) so the OTLP translator only depends on the storage
/// shape, not on any of the in-flight wire shapes.
#[derive(Debug, Clone)]
pub struct StoredSpan {
    pub id: String,
    pub run_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub t0_ms: i64,
    pub duration_ms: Option<i64>,
    pub attrs_json: String,
    pub prompt: Option<String>,
    pub response: Option<String>,
}

/// Hash a string into a hex prefix of the requested *byte* length.
/// SHA-256 is overkill but guarantees collision resistance across
/// the lifetime of the app, and it's already in the dep tree via
/// `sqlx-core`'s migrator (free transitive).
fn hex_prefix(input: &str, byte_len: usize) -> String {
    let digest = Sha256::digest(input.as_bytes());
    // `byte_len` raw bytes → `byte_len * 2` hex chars. Slice rather
    // than re-encoding so the work scales with `byte_len`, not the
    // full SHA-256 output.
    let take = byte_len.min(digest.len());
    let mut out = String::with_capacity(take * 2);
    for b in &digest[..take] {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// `traceId`: deterministic 16-byte hex of `sha256(run_id)`. Same
/// `run_id` always hashes to the same trace id so retries don't
/// fragment a logical run across multiple traces in the collector.
pub fn trace_id_for(run_id: &str) -> String {
    hex_prefix(run_id, TRACE_ID_LEN)
}

/// `spanId`: deterministic 8-byte hex of `sha256(span_id)`.
pub fn span_id_for(span_id: &str) -> String {
    hex_prefix(span_id, SPAN_ID_LEN)
}

/// Build the full OTLP envelope for a batch of spans.
///
/// Returns `(envelope, ids)` — the JSON value to POST, and the
/// vector of database `id`s that the caller flips to `exported_at`
/// on a 2xx. The pair is returned together so the exporter doesn't
/// have to walk the spans twice.
pub fn build_envelope(spans: &[StoredSpan]) -> Result<(Value, Vec<String>), AppError> {
    let mut otlp_spans = Vec::with_capacity(spans.len());
    let mut ids = Vec::with_capacity(spans.len());
    for s in spans {
        otlp_spans.push(span_to_otlp(s)?);
        ids.push(s.id.clone());
    }

    let envelope = serde_json::json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    {
                        "key": "service.name",
                        "value": { "stringValue": SERVICE_NAME }
                    }
                ]
            },
            "scopeSpans": [{
                "scope": { "name": SCOPE_NAME },
                "spans": otlp_spans,
            }]
        }]
    });
    Ok((envelope, ids))
}

/// Translate one stored span into its OTLP/HTTP/JSON v1.3 shape.
///
/// Skipped behaviours (per WP §"Out of scope"):
/// - resource attributes beyond `service.name` (handled at envelope level)
/// - `Span.events` / `Span.links` (unused by Neuron's runtime)
fn span_to_otlp(s: &StoredSpan) -> Result<Value, AppError> {
    // Time fields — OTLP wants `*UnixNano` as a string-typed uint64
    // (the proto's `fixed64` JSON encoding is a string to dodge
    // JS-number precision loss). `t0_ms * 1_000_000` widens the ms
    // to ns; we emit it as a string for the same reason.
    let start_ns = (s.t0_ms as i128) * 1_000_000;
    let end_ns = s
        .duration_ms
        .map(|d| (s.t0_ms as i128 + d as i128) * 1_000_000);

    // Parse the stored `attrs_json` into a flat OTLP attribute list.
    // Bad JSON falls back to an empty list rather than aborting the
    // whole batch — we want the export sweep to be lossy-but-running
    // over malformed historical rows, not stuck.
    let mut attrs = json_object_to_attrs(&s.attrs_json);
    let status_code = if attrs_indicate_error(&s.attrs_json) {
        STATUS_ERROR
    } else {
        STATUS_OK
    };

    // gen_ai.prompt / gen_ai.completion semconv — truncate to keep
    // the envelope bounded. WP §"Scope": "truncated to 1 KiB".
    if let Some(p) = &s.prompt {
        attrs.push(string_attr("gen_ai.prompt", &truncate_chars(p, ATTR_TEXT_CAP)));
    }
    if let Some(r) = &s.response {
        attrs.push(string_attr(
            "gen_ai.completion",
            &truncate_chars(r, ATTR_TEXT_CAP),
        ));
    }

    let mut span = Map::new();
    span.insert("traceId".into(), Value::String(trace_id_for(&s.run_id)));
    span.insert("spanId".into(), Value::String(span_id_for(&s.id)));
    if let Some(parent) = &s.parent_span_id {
        span.insert("parentSpanId".into(), Value::String(span_id_for(parent)));
    }
    span.insert("name".into(), Value::String(s.name.clone()));
    span.insert("kind".into(), Value::from(KIND_INTERNAL));
    span.insert(
        "startTimeUnixNano".into(),
        Value::String(start_ns.to_string()),
    );
    if let Some(end) = end_ns {
        span.insert("endTimeUnixNano".into(), Value::String(end.to_string()));
    }
    if !attrs.is_empty() {
        span.insert("attributes".into(), Value::Array(attrs));
    }
    span.insert(
        "status".into(),
        serde_json::json!({ "code": status_code }),
    );
    Ok(Value::Object(span))
}

/// Build a `{ key, value: { stringValue } }` attribute object.
fn string_attr(key: &str, value: &str) -> Value {
    serde_json::json!({
        "key": key,
        "value": { "stringValue": value }
    })
}

/// Build a `{ key, value: { intValue } }` attribute object. OTLP
/// uses string-typed int64s for the same reason the timestamps do.
fn int_attr(key: &str, value: i64) -> Value {
    serde_json::json!({
        "key": key,
        "value": { "intValue": value.to_string() }
    })
}

/// Build a `{ key, value: { doubleValue } }` attribute object.
fn double_attr(key: &str, value: f64) -> Value {
    serde_json::json!({
        "key": key,
        "value": { "doubleValue": value }
    })
}

/// Build a `{ key, value: { boolValue } }` attribute object.
fn bool_attr(key: &str, value: bool) -> Value {
    serde_json::json!({
        "key": key,
        "value": { "boolValue": value }
    })
}

/// Convert one `attrs_json` blob into a Vec of OTLP attributes.
/// Non-object inputs and parse errors yield an empty Vec — see
/// note above about "lossy-but-running".
fn json_object_to_attrs(raw: &str) -> Vec<Value> {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(obj) = parsed.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(obj.len());
    for (k, v) in obj {
        match v {
            Value::String(s) => out.push(string_attr(k, s)),
            Value::Bool(b) => out.push(bool_attr(k, *b)),
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    out.push(int_attr(k, i));
                } else if let Some(f) = n.as_f64() {
                    out.push(double_attr(k, f));
                }
            }
            // Nested objects/arrays are flattened to a JSON-string
            // attribute. OTLP's `arrayValue`/`kvlistValue` exists
            // but the WP scope is single-level; a flat string keeps
            // the wire small and the collector can still parse it.
            Value::Object(_) | Value::Array(_) => {
                if let Ok(s) = serde_json::to_string(v) {
                    out.push(string_attr(k, &s));
                }
            }
            Value::Null => {}
        }
    }
    out
}

/// Heuristic: a span is "errored" if its attrs_json carries a
/// non-empty `error` field (any type) or `status` equals `"error"`.
/// Conservative: missing fields default to OK.
fn attrs_indicate_error(raw: &str) -> bool {
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let Some(obj) = parsed.as_object() else {
        return false;
    };
    if let Some(err) = obj.get("error") {
        if !err.is_null() {
            // Treat empty strings / empty objects as "no error" so a
            // span that opportunistically writes `"error": ""` does
            // not flip status.
            match err {
                Value::String(s) => return !s.is_empty(),
                Value::Object(o) => return !o.is_empty(),
                _ => return true,
            }
        }
    }
    if let Some(Value::String(s)) = obj.get("status") {
        return s == "error";
    }
    false
}

/// Truncate `s` to at most `cap` chars (UTF-8-safe — splits on a
/// codepoint boundary). Used for prompt/completion payloads.
fn truncate_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    s.chars().take(cap).collect()
}


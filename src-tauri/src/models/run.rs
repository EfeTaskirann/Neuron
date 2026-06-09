//! Run + span domain types (`runs`/`runs_spans` + filter/detail).

use serde::{Deserialize, Serialize};
use specta::Type;

/// One row of `runs`. Field renames map the storage column names to
/// the keys the frontend mock uses (`workflow`, `dur`, `cost`).
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    /// Denormalized workflow display name. Mock key: `workflow`.
    #[serde(rename = "workflow")]
    pub workflow_name: String,
    /// Original workflow id (FK target). Carried so the inspector
    /// can deep-link without an extra `runs:get` round-trip.
    pub workflow_id: String,
    /// Unix epoch seconds. Mock key in `data.js` is the formatted
    /// relative-time string `started`; the wire format here is a
    /// number to stay type-stable, and the frontend hook formats it
    /// for display.
    pub started_at: i64,
    /// Mock key: `dur` (milliseconds). `Option` because a still-
    /// running run has no duration yet.
    #[serde(rename = "dur")]
    pub duration_ms: Option<i64>,
    /// Mock key: `tokens`.
    pub tokens: i64,
    /// Mock key: `cost`.
    #[serde(rename = "cost")]
    pub cost_usd: f64,
    /// `'running'|'success'|'error'` (CHECK-constrained at SQL).
    pub status: String,
}

/// Optional filter for `runs:list`. Currently scopes by status only;
/// extended by WP-04 once real run execution lands.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RunFilter {
    pub status: Option<String>,
    pub workflow_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub id: String,
    pub run_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    /// `'llm'|'tool'|'logic'|'human'|'http'`.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub span_type: String,
    pub t0_ms: i64,
    pub duration_ms: Option<i64>,
    pub attrs_json: String,
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub is_running: bool,
    /// Tree depth derived from `parent_span_id` at read time via the
    /// recursive CTE in `runs:get`. Not stored. `#[sqlx(default)]`
    /// guards against ad-hoc SELECTs that omit the computed column;
    /// no `serde(default)` because `Span` is never deserialised from
    /// JSON (events use `WireSpan`), and we want the TypeScript field
    /// to stay required for frontend consumers.
    #[sqlx(default)]
    pub indent: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    pub run: Run,
    pub spans: Vec<Span>,
}

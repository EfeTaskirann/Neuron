//! Unified application error type for the Tauri command surface.
//!
//! Per WP-W2-03 § "Notes / risks":
//!
//!   Errors: return `Result<T, AppError>`. AppError variants:
//!   `NotFound`, `Conflict`, `InvalidInput`, `DbError`, `Internal`.
//!   Serialized form: `{ "kind": "not_found", "message": "Agent abc not found" }`.
//!
//! The discriminant is emitted as snake_case (`not_found`) and the
//! human-readable message rides alongside. The frontend pattern-matches
//! on `kind` for behaviour (e.g., 404 → "no record" empty state) and
//! shows `message` to users for debugging.
//!
//! `tauri_specta`/`specta::Type` is required so the generated
//! `bindings.ts` can emit a typed `Result<T, AppError>` for every
//! command return. Without `Type`, the bindings would degrade to
//! `Promise<T | string>` which silently breaks shape parity with the
//! frontend mock.
//!
//! The wire shape we produce — a tagged object `{ kind, message }` —
//! is intentionally hand-rolled instead of leaning on serde's
//! internally-tagged enum. specta does not yet model serde's `tag = ...`
//! directive faithfully across all phases, so we serialize manually
//! and declare the matching `Type` impl below.

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use specta::Type;
use thiserror::Error;

/// Errors surfaced to the frontend by every Tauri command.
///
/// Variants map 1:1 to a stable `kind` string seen by JS callers:
///
/// | Variant       | `kind`         | Typical cause                              |
/// |---------------|----------------|--------------------------------------------|
/// | `NotFound`    | `not_found`    | `agents:get` / `runs:get` / `mcp:install` etc. on an unknown id |
/// | `Conflict`    | `conflict`     | unique constraint, double-cancel, double-emit |
/// | `InvalidInput`| `invalid_input`| empty required field, bad temp range, etc. |
/// | `DbError`     | `db_error`     | sqlx/SQLite returned an error              |
/// | `Internal`    | `internal`     | catch-all; bug or environment failure      |
/// | `Sidecar`     | `sidecar`      | LangGraph Python sidecar pipe / spawn fail |
/// | `NoApiKey`    | `no_api_key`   | provider key missing from OS keychain      |
#[derive(Debug, Clone, Error, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum AppError {
    /// Resource lookup miss (non-existent id, no rows returned, etc.).
    #[error("not found: {0}")]
    NotFound(String),

    /// Mutation rejected — duplicate, race, or invalid state transition.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Caller-supplied data failed validation before touching the DB.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// `sqlx` / SQLite returned an error. The nested `String` is the
    /// `Display` form of the underlying error so the JS console gets a
    /// readable message even though the original `sqlx::Error` is not
    /// `Serialize`.
    #[error("database error: {0}")]
    DbError(String),

    /// LangGraph Python sidecar failed — spawn error, broken stdio
    /// pipe, JSON decode error, or sidecar process not yet ready when
    /// a `runs:create` came in. The frontend pattern-matches on
    /// `kind='sidecar'` to surface a "agent runtime offline" CTA.
    #[error("sidecar error: {0}")]
    Sidecar(String),

    /// Provider API key not configured in the OS keychain. The
    /// `provider` discriminant is the user-visible name shown by the
    /// "Configure API keys" CTA in Settings (`anthropic`, `openai`).
    /// The wire form is `{ "kind": "no_api_key", "message": "anthropic" }`.
    #[error("no API key configured for {0}")]
    NoApiKey(String),

    /// MCP server JSON-RPC failure — bad frame, unexpected response,
    /// timeout. Distinct from `Sidecar` because the MCP boundary uses
    /// newline-delimited JSON-RPC 2.0 instead of WP-W2-04's length-
    /// prefixed framing, and the user-facing recovery is different
    /// (re-install the MCP server vs. restart the agent runtime).
    #[error("mcp protocol error: {0}")]
    McpProtocol(String),

    /// MCP server failed to spawn — `npx` not on PATH, package
    /// missing, child died before initialize completed. The frontend
    /// pattern-matches on `kind='mcp_server_spawn_failed'` to surface
    /// "Install Node.js / fix PATH" guidance.
    #[error("mcp server spawn failed: {0}")]
    McpServerSpawnFailed(String),

    /// WP-W3-11 — `claude` CLI binary could not be located on this
    /// host. The message embeds the resolution chain (env override
    /// probed, `which::which` result, platform-specific fallbacks
    /// inspected) plus a CTA pointing at the official setup docs.
    /// Frontend pattern-matches on `kind='claude_binary_missing'` to
    /// surface a "run `claude login` / install Claude Code" banner.
    #[error("claude binary missing: {0}")]
    ClaudeBinaryMissing(String),

    /// WP-W3-11 — the `claude` subprocess emitted a `result` event of
    /// subtype `error`, or exited non-zero before producing a
    /// `result.success`. The nested string carries either the model-
    /// reported reason or the stderr ring tail surfaced by
    /// `SubprocessTransport::invoke`.
    #[error("swarm invoke error: {0}")]
    SwarmInvoke(String),

    /// WP-W3-11 — a wrapped operation ran past its budget. Currently
    /// emitted only by `SubprocessTransport::invoke` when the stdout
    /// read loop's `tokio::time::timeout` expires; the child is
    /// killed via `kill_on_drop` on the way out. Future WPs may
    /// reuse this for any other budget-bound wait.
    #[error("operation timed out: {0}")]
    Timeout(String),

    /// WP-W3-12a — `swarm:run_job` was called with a `workspace_id`
    /// that already has an in-flight job. Per the owner directive
    /// 2026-05-05 ("aynı proje için yeni bir 9 kişilik ekibi
    /// çalıştırmama izin vermesin"), same workspace serializes;
    /// different workspaces run in parallel. Carries both the
    /// workspace_id the caller supplied and the job_id of the
    /// currently-running job so the UI can surface "wait for job X
    /// to finish" without an extra IPC round-trip.
    #[error("workspace `{workspace_id}` busy with job `{in_flight_job_id}`")]
    WorkspaceBusy {
        workspace_id: String,
        in_flight_job_id: String,
    },

    /// WP-W4-01 — a wrapped operation observed a cancel signal before
    /// completing. Surfaced by `PersistentSession::invoke_turn` when
    /// its cancel `Notify` fires mid-read. Distinct from `Timeout`
    /// (budget elapsed) and `SwarmInvoke` (subprocess error) so the
    /// caller can branch — typically: cancel → leave session alive,
    /// timeout / invoke-error → consider session unhealthy.
    /// Frontend pattern-matches on `kind='cancelled'` to surface a
    /// neutral "İptal edildi" indicator instead of the red error
    /// banner.
    #[error("cancelled: {0}")]
    Cancelled(String),

    /// Catch-all for unclassified failures (panics-in-tasks, missing
    /// env, etc.). Frontend treats `internal` as a developer bug.
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    /// Stable kebab-case discriminant used in the wire format. Mirrors
    /// the `serde(rename_all = "snake_case")` directive but exposed as
    /// a function so command-side tests can assert the shape without
    /// re-serializing.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::Conflict(_) => "conflict",
            Self::InvalidInput(_) => "invalid_input",
            Self::DbError(_) => "db_error",
            Self::Sidecar(_) => "sidecar",
            Self::NoApiKey(_) => "no_api_key",
            Self::McpProtocol(_) => "mcp_protocol",
            Self::McpServerSpawnFailed(_) => "mcp_server_spawn_failed",
            Self::ClaudeBinaryMissing(_) => "claude_binary_missing",
            Self::SwarmInvoke(_) => "swarm_invoke",
            Self::Timeout(_) => "timeout",
            Self::WorkspaceBusy { .. } => "workspace_busy",
            Self::Cancelled(_) => "cancelled",
            Self::Internal(_) => "internal",
        }
    }

    /// Human-readable detail. For the single-string variants this is
    /// the inner payload verbatim; for `WorkspaceBusy` (struct
    /// variant) we synthesize the same text the `Display` impl
    /// produces. Returning `Cow<str>` keeps callers cheap on the
    /// common single-string path while still letting the struct
    /// variant render without leaking owned memory.
    pub fn message(&self) -> std::borrow::Cow<'_, str> {
        use std::borrow::Cow;
        match self {
            Self::NotFound(m)
            | Self::Conflict(m)
            | Self::InvalidInput(m)
            | Self::DbError(m)
            | Self::Sidecar(m)
            | Self::NoApiKey(m)
            | Self::McpProtocol(m)
            | Self::McpServerSpawnFailed(m)
            | Self::ClaudeBinaryMissing(m)
            | Self::SwarmInvoke(m)
            | Self::Timeout(m)
            | Self::Cancelled(m)
            | Self::Internal(m) => Cow::Borrowed(m.as_str()),
            Self::WorkspaceBusy {
                workspace_id,
                in_flight_job_id,
            } => Cow::Owned(format!(
                "workspace `{workspace_id}` busy with job `{in_flight_job_id}`"
            )),
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // `{ "kind": "not_found", "message": "..." }` — flat wire
        // shape. Manual to keep parity with the `specta::Type` impl
        // below; serde's internally-tagged enums currently confuse the
        // typescript exporter for the `content` field.
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("kind", self.kind())?;
        s.serialize_field("message", self.message().as_ref())?;
        s.end()
    }
}

/// Helper struct used purely to anchor the `specta::Type` derive on
/// the wire shape we hand-serialize above. Without this, `bindings.ts`
/// would not see `AppError` at all and frontend `Result<T, AppError>`
/// would degrade to `Result<T, unknown>`.
#[derive(Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct AppErrorWire {
    kind: String,
    message: String,
}

impl Type for AppError {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        // Reuse the helper struct's Type impl so the emitted TS is
        // exactly `{ kind: string; message: string }`.
        AppErrorWire::definition(types)
    }
}

// ---------------------------------------------------------------------
// `From` conversions — the command modules use `?` against sqlx and
// std types and expect them to widen to `AppError` automatically.
// ---------------------------------------------------------------------

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            // RowNotFound surfaces as 404 — sqlx returns this from
            // `fetch_one` when zero rows match, which is the precise
            // semantic of `NotFound`.
            sqlx::Error::RowNotFound => AppError::NotFound("row not found".into()),
            // Unique constraint violation is the canonical conflict.
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                AppError::Conflict(db.message().to_string())
            }
            _ => AppError::DbError(e.to_string()),
        }
    }
}

impl From<sqlx::migrate::MigrateError> for AppError {
    fn from(e: sqlx::migrate::MigrateError) -> Self {
        AppError::DbError(e.to_string())
    }
}

impl From<tauri::Error> for AppError {
    fn from(e: tauri::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("serde_json: {e}"))
    }
}

#[cfg(test)]
mod tests {
    //! Acceptance: AppError serializes to `{ kind, message }` exactly
    //! and round-trips back through serde without information loss.

    use super::*;

    #[test]
    fn serializes_to_kind_message_object() {
        let err = AppError::NotFound("Agent abc not found".to_string());
        let json = serde_json::to_value(&err).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "not_found",
                "message": "Agent abc not found",
            })
        );
    }

    #[test]
    fn discriminant_matches_kind_method() {
        assert_eq!(AppError::NotFound("x".into()).kind(), "not_found");
        assert_eq!(AppError::Conflict("x".into()).kind(), "conflict");
        assert_eq!(AppError::InvalidInput("x".into()).kind(), "invalid_input");
        assert_eq!(AppError::DbError("x".into()).kind(), "db_error");
        assert_eq!(AppError::Internal("x".into()).kind(), "internal");
    }

    #[test]
    fn sqlx_row_not_found_maps_to_not_found() {
        let err: AppError = sqlx::Error::RowNotFound.into();
        assert_eq!(err.kind(), "not_found");
    }
}

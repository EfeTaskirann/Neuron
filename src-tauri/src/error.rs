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
            Self::Internal(_) => "internal",
        }
    }

    /// Human-readable detail. Same content as the `Display` impl's
    /// payload but without the variant prefix.
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(m)
            | Self::Conflict(m)
            | Self::InvalidInput(m)
            | Self::DbError(m)
            | Self::Internal(m) => m,
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
        s.serialize_field("message", self.message())?;
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

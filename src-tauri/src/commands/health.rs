//! `health` command namespace — DB smoke test.
//!
//! Why this exists
//! ---------------
//! The WP-W2-02 acceptance criteria require "DbPool exposed via
//! `tauri::State<DbPool>` and accessible from a smoke command".
//! Real domain commands ship in WP-W2-03; this single command is
//! the orchestrator's evidence that the pool wiring is reachable
//! from the IPC layer at all.
//!
//! Naming note
//! -----------
//! The Charter's command surface uses colon-namespaced names
//! (`agents:list`, `runs:list`, …). Rust function identifiers can
//! not contain `:`, and Tauri 2.x's `#[command]` attribute does not
//! ship a stable `rename = "..."` argument we can lean on without
//! pulling in extra crates. Per WP-W2-02 explicit guidance the
//! `health_db` underscore form is acceptable for this WP only —
//! WP-W2-03 introduces specta-driven binding generation which will
//! alias the IPC surface back to the colon form.

use serde::Serialize;
use sqlx::Row;
use tauri::State;

use crate::db::DbPool;

/// Health payload returned to the frontend. Field names use
/// camelCase so the future TS bindings need no remapping.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbHealth {
    /// Number of user tables in the schema (excludes sqlx + sqlite
    /// internal tables). Should be 11 after WP-W2-02 migration.
    pub tables: i64,
    /// Mirrors `PRAGMA foreign_keys` for the connection used to
    /// answer this call. Must be `true` for the wiring to be sane.
    pub foreign_keys_on: bool,
}

#[tauri::command]
pub async fn health_db(pool: State<'_, DbPool>) -> Result<DbHealth, String> {
    let pool = pool.inner();

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master \
         WHERE type='table' AND name NOT LIKE 'sqlite_%' \
           AND name NOT LIKE '_sqlx_%'",
    )
    .fetch_one(pool)
    .await
    .map_err(|e| format!("health_db: count tables failed: {e}"))?;

    let row = sqlx::query("PRAGMA foreign_keys")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("health_db: read pragma failed: {e}"))?;
    // PRAGMA foreign_keys returns a single nameless integer column.
    let fk: i64 = row
        .try_get(0)
        .map_err(|e| format!("health_db: pragma column missing: {e}"))?;

    Ok(DbHealth {
        tables,
        foreign_keys_on: fk == 1,
    })
}

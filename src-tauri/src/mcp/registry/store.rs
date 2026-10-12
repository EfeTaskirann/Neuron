//! SQLite persistence for the MCP registry.
//!
//! Every `servers` / `server_tools` statement lives here so [`super`]
//! stays a thin lifecycle-orchestration layer: the install
//! transaction, the uninstall flag-flip, the installed-flag probe, and
//! the persisted tool-row read.

use crate::db::DbPool;
use crate::error::AppError;
use crate::mcp::client::ToolDescriptor;
use crate::mcp::manifests::ServerManifest;
use crate::models::Server;

/// Persist `installed=1` and the tool list in one transaction.
pub(super) async fn persist_install(
    pool: &DbPool,
    manifest: &ServerManifest,
    tools: &[ToolDescriptor],
) -> Result<Server, AppError> {
    let mut tx = pool.begin().await?;
    // Drop any stale tools first (idempotent reinstall).
    sqlx::query("DELETE FROM server_tools WHERE server_id = ?")
        .bind(&manifest.id)
        .execute(&mut *tx)
        .await?;
    for tool in tools {
        let schema_json = tool.input_schema.to_string();
        sqlx::query(
            "INSERT INTO server_tools (server_id, name, description, input_schema_json) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&manifest.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(schema_json)
        .execute(&mut *tx)
        .await?;
    }
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 1 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(&manifest.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {} not seeded", manifest.id)))?;
    tx.commit().await?;
    Ok(updated)
}

/// Drop a server's tools and flip `installed=0` in one transaction
/// (mirrors `persist_install` — a failure between the two statements
/// must not leave an installed server with no tool rows).
pub(super) async fn uninstall_server(pool: &DbPool, id: &str) -> Result<Server, AppError> {
    let mut tx = pool.begin().await?;
    // The FK on server_tools.server_id cascades on delete of `servers`,
    // but we only flip a flag — explicit `DELETE FROM server_tools` is
    // the right shape here.
    sqlx::query("DELETE FROM server_tools WHERE server_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 0 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;
    tx.commit().await?;
    Ok(updated)
}

/// Read the `installed` flag for one server. `None` = no such row.
pub(super) async fn read_installed_flag(
    pool: &DbPool,
    id: &str,
) -> Result<Option<bool>, AppError> {
    let installed = sqlx::query_scalar::<_, bool>(
        "SELECT installed FROM servers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(installed)
}

/// Read the persisted tools for one server.
pub(super) async fn read_tools(
    pool: &DbPool,
    id: &str,
) -> Result<Vec<crate::models::Tool>, AppError> {
    // We accept calls against any id, even uninstalled ones — the
    // resulting empty list is the right answer ("no tools registered").
    let rows = sqlx::query_as::<_, crate::models::Tool>(
        "SELECT server_id, name, description, input_schema_json \
         FROM server_tools WHERE server_id = ? ORDER BY name",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

//! MCP server registry — install / uninstall / call orchestration.
//!
//! Glue layer between [`crate::mcp::manifests`] (catalog metadata),
//! [`crate::mcp::client`] (one-shot stdio sessions), and the SQLite
//! `servers` / `server_tools` rows.
//!
//! ## Lifecycle
//!
//! - `install(pool, app, id)`:
//!     1. Look up the manifest. Reject unknown ids with `NotFound`.
//!     2. Verify the manifest is fully wired (`spawn` is set). Stub
//!        manifests (browser, slack, vector-db, postgres) error out
//!        with [`AppError::McpServerSpawnFailed`] until Week 3 adds a
//!        sandbox + secret-prompt flow.
//!     3. Resolve the spawn template (substitute `__ROOT__` for
//!        Filesystem) and any required secret env var.
//!     4. Spawn the server, perform the handshake, run `tools/list`.
//!     5. Persist `installed=1` plus one row per tool in
//!        `server_tools`. Return the updated `Server` row.
//!     6. `shutdown()` the client so the spawned `npx` exits cleanly.
//!
//! - `uninstall(pool, id)` flips `installed=0` and deletes
//!   `server_tools` for that id (FK cascades on the row).
//!
//! - `call_tool(pool, app, id, name, args)` re-spawns the server
//!   (no pooling in Week 2 — see WP §"Out of scope"), calls the
//!   tool, shuts down. Returns the [`CallToolOutput`] verbatim.
//!
//! ## Why no session pool
//!
//! The WP body explicitly defers pooling to Week 3 (alongside
//! sandbox isolation). Each `mcp:callTool` is therefore a full
//! spawn cycle — slow, but correct and trivially auditable.
//!
//! ## Module layout
//!
//! This file is the public lifecycle API only — a thin orchestration
//! layer. The two implementation axes live in private submodules:
//!
//! - [`spawn`] — manifest → connected [`McpClient`]: command/arg
//!   templating, `__ROOT__` substitution, secret-env resolution, the
//!   npx executable name.
//! - [`store`] — every `servers` / `server_tools` SQL statement: the
//!   install transaction, the uninstall flag-flip, and the
//!   installed-flag / tool-row reads.

mod spawn;
mod store;

use serde_json::Value;
use tauri::{AppHandle, Runtime};

use crate::db::DbPool;
use crate::error::AppError;
use crate::mcp::client::CallToolOutput;
use crate::mcp::manifests;
use crate::models::Server;

/// Install an MCP server: spawn it, list its tools, persist them, and
/// flip the `installed` flag.
pub async fn install<R: Runtime>(
    pool: &DbPool,
    app: &AppHandle<R>,
    id: &str,
) -> Result<Server, AppError> {
    let manifest = manifests::get(id)
        .map_err(|e| AppError::Internal(format!("manifest load: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;

    // Connect, list, and persist tools — then flip the flag in one
    // transaction so a half-installed state is never observable.
    let tools = spawn::fetch_tools(app, &manifest).await?;
    store::persist_install(pool, &manifest, &tools).await
}

/// Uninstall: drop tools, flip flag.
pub async fn uninstall(pool: &DbPool, id: &str) -> Result<Server, AppError> {
    store::uninstall_server(pool, id).await
}

/// Read the persisted tools for one server. Returns the raw rows the
/// agent runtime (WP-W2-04) consumes when planning calls.
pub async fn list_tools(pool: &DbPool, id: &str) -> Result<Vec<crate::models::Tool>, AppError> {
    store::read_tools(pool, id).await
}

/// Call one tool by re-spawning the server. Slow but correct in
/// Week 2; pooling moves the spawn out of the hot path in Week 3.
pub async fn call_tool<R: Runtime>(
    pool: &DbPool,
    app: &AppHandle<R>,
    id: &str,
    tool_name: &str,
    args: Value,
) -> Result<CallToolOutput, AppError> {
    // Verify the server is installed before paying for an `npx` cold
    // start — the alternative is the user gets a confusing
    // `McpServerSpawnFailed` for a row that exists but is uninstalled.
    match store::read_installed_flag(pool, id).await? {
        None => return Err(AppError::NotFound(format!("Server {id} not found"))),
        Some(false) => {
            return Err(AppError::Conflict(format!(
                "Server {id} is not installed"
            )))
        }
        Some(true) => {}
    }
    let manifest = manifests::get(id)
        .map_err(|e| AppError::Internal(format!("manifest load: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Manifest {id} not found")))?;
    let mut client = spawn::spawn_for_manifest(app, &manifest).await?;
    let out = client.call_tool(tool_name, args).await;
    client.shutdown().await;
    out
}

#[cfg(test)]
mod tests;

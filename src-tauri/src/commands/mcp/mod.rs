//! `mcp:*` namespace.
//!
//! - `mcp:list`        → `Server[]`
//! - `mcp:install`   `(id)` → `Server` — spawns the server, runs
//!                                       `tools/list`, persists the
//!                                       tools, flips `installed=1`.
//! - `mcp:uninstall` `(id)` → `Server` — drops tools + flips flag.
//! - `mcp:listTools` `(serverId)` → `Tool[]` — used by the agent
//!                                            runtime (WP-W2-04).
//! - `mcp:callTool`  `(serverId, name, args)` → `CallToolResult`
//!                  — re-spawns the server, calls one tool, returns
//!                  the MCP-spec response shape.
//!
//! ## Events
//!
//! Per ADR-0006, `mcp:install` emits `mcp:installed` and
//! `mcp:uninstall` emits `mcp:uninstalled` (logical names use `.` per
//! the ADR; Tauri 2.10 forbids `.` so the wire form swaps in `:`).
//! Frontend hooks invalidate the `['mcp']` cache key on either signal.
//!
//! ## Stubbed manifests
//!
//! Postgres / Browser / Slack / Vector DB are seeded as catalog rows
//! with no `spawn` recipe. Calling `mcp:install` on them surfaces
//! [`AppError::McpServerSpawnFailed`] so the frontend can render a
//! "Coming soon" CTA without the row mysteriously not toggling.
//!
//! ## Module layout
//!
//! - `mod.rs` (this file) — the five `#[tauri::command]` handlers.
//! - `tests.rs` — the `#[cfg(test)] mod tests` acceptance suite. Split
//!   out (mirroring `commands/agents/` and `commands/terminal/`) so
//!   this file stays pure production surface; `use super::*` there
//!   still resolves to this module unchanged.

use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::mcp::registry;
use crate::models::{CallToolResult, Server, Tool, ToolContent};

#[tauri::command]
#[specta::specta]
pub async fn mcp_list(pool: State<'_, DbPool>) -> Result<Vec<Server>, AppError> {
    let rows = sqlx::query_as::<_, Server>(
        "SELECT id, name, by, description, installed FROM servers ORDER BY name, id",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_install<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<Server, AppError> {
    let updated = registry::install(pool.inner(), &app, &id).await?;
    // Wire-name constant lives in `crate::events` (ADR-0006 § "Wire-
    // format substitution" — the logical `mcp.installed` is on-wire
    // `mcp:installed`).
    app.emit(events::MCP_INSTALLED, &updated)?;
    Ok(updated)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_uninstall<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<Server, AppError> {
    let updated = registry::uninstall(pool.inner(), &id).await?;
    app.emit(events::MCP_UNINSTALLED, &updated)?;
    Ok(updated)
}

/// `mcp:listTools(serverId)` — return persisted tools for one server.
/// Empty list for uninstalled or unknown servers (the agent runtime
/// treats that as "no capabilities", which is the correct fallback).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_list_tools(
    pool: State<'_, DbPool>,
    server_id: String,
) -> Result<Vec<Tool>, AppError> {
    registry::list_tools(pool.inner(), &server_id).await
}

/// `mcp:callTool(serverId, name, argsJson)` — execute one tool
/// against a freshly-spawned server. `argsJson` is the JSON-stringified
/// argument object; the server validates it against its `inputSchema`
/// and surfaces any schema violation as a structured `isError=true`
/// result.
///
/// Why a string instead of a typed object: the tool's argument shape
/// is whatever the MCP server declares, which the Rust side has no
/// compile-time knowledge of. Passing JSON as a `String` keeps the
/// `bindings.ts` signature clean (`argsJson: string`) — the caller is
/// expected to do `JSON.stringify(args)` at the boundary, and the
/// frontend `useMcpCallTool` hook (Week 3) will hide that cermony.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mcp_call_tool<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    server_id: String,
    name: String,
    args_json: String,
) -> Result<CallToolResult, AppError> {
    let args: serde_json::Value = if args_json.is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str(&args_json).map_err(|e| {
            AppError::InvalidInput(format!("argsJson must be valid JSON: {e}"))
        })?
    };
    let out = registry::call_tool(pool.inner(), &app, &server_id, &name, args).await?;
    // Translate the client's `CallToolOutput` into the typed wire
    // shape. The two structs are isomorphic — keeping a separate
    // `models::*` form lets the frontend `bindings.ts` import the
    // shape without depending on `crate::mcp::*` internals.
    let content = out
        .content
        .into_iter()
        .map(|b| match b {
            crate::mcp::client::ContentBlock::Text { text } => ToolContent::Text { text },
            crate::mcp::client::ContentBlock::Other => ToolContent::Other,
        })
        .collect();
    Ok(CallToolResult {
        content,
        is_error: out.is_error,
    })
}

#[cfg(test)]
mod tests;

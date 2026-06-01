//! `swarm:agents:list_status` + `swarm:agents:shutdown_workspace` —
//! WP-W4-02 read + lifecycle surface on the `SwarmAgentRegistry`.

use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::swarm::{AgentStatusRow, SwarmAgentRegistry};

/// Read-only snapshot of every agent's status for `workspace_id`.
/// One row per bundled / workspace-override profile (9 rows on a
/// fresh install). The eventual W4-04 grid header drives off this
/// shape.
///
/// Validation: `workspace_id.trim().is_empty()` → `InvalidInput`.
/// Missing registry state → `Internal` (defensive; `lib.rs::setup`
/// always installs the registry on production runs).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_list_status<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<Vec<AgentStatusRow>, AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    let registry = app
        .try_state::<Arc<SwarmAgentRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "SwarmAgentRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    Ok(registry.list_status(&workspace_id).await)
}

/// Eager shutdown of every session for `workspace_id`. Idempotent;
/// calling on an empty workspace returns `Ok(())`. Used by the
/// W4-04 UI's "End swarm" affordance and (eventually) by the
/// app-close lifecycle in `lib.rs`.
///
/// Validation: `workspace_id.trim().is_empty()` → `InvalidInput`.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_shutdown_workspace<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<(), AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    let registry = app
        .try_state::<Arc<SwarmAgentRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "SwarmAgentRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    registry.shutdown_workspace(&workspace_id).await
}

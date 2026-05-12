//! Terminal-Hierarchy Swarm IPC surface.
//!
//! Phase 1: `swarm_term:list_personas` returns the 9 bundled personas
//! used by the terminal-swarm UI (header chips, persona preview).
//! Spawn / route / hierarchy IPCs land in Phases 2-6.

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::swarm::ProfileRegistry;
use crate::swarm_term::TerminalSwarmSessionHandle;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SwarmTermPersona {
    pub id: String,
    pub role: String,
    pub description: String,
    pub allowed_destinations: Vec<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_list_personas<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<SwarmTermPersona>, AppError> {
    let workspace_dir = app
        .path()
        .app_data_dir()
        .ok()
        .map(|p| p.join("swarm-term").join("agents"))
        .filter(|p| p.is_dir());
    let registry = ProfileRegistry::load_term(workspace_dir.as_deref())?;
    let mut out: Vec<SwarmTermPersona> = registry
        .list()
        .into_iter()
        .filter(|p| crate::swarm_term::hierarchy::AGENT_IDS.contains(&p.id.as_str()))
        .map(|p| SwarmTermPersona {
            id: p.id.clone(),
            role: p.role.clone(),
            description: p.description.clone(),
            allowed_destinations: crate::swarm_term::hierarchy::allowed_for(&p.id)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_session_status<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<TerminalSwarmSessionHandle>, AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>();
    Ok(registry.current())
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_term_start_session<R: Runtime>(
    app: AppHandle<R>,
    project_dir: String,
) -> Result<TerminalSwarmSessionHandle, AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>()
        .inner()
        .clone();
    let path = std::path::PathBuf::from(project_dir.trim());
    registry.start(app.clone(), path).await
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_stop_session<R: Runtime>(
    app: AppHandle<R>,
) -> Result<(), AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>()
        .inner()
        .clone();
    registry.stop(app.clone()).await
}

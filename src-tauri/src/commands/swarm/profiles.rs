//! `swarm:profiles_list` + `swarm:test_invoke` — the WP-W3-11 surface
//! that proves the bundled `claude` subprocess pipe is healthy.
//!
//! Both commands resolve the workspace-override directory from
//! `app_data_dir`'s `agents/` subdirectory (via [`workspace_agents_dir`])
//! and pass it (optionally) into [`ProfileRegistry::load_from`] —
//! bundled profiles are read unconditionally via `include_dir!` inside
//! the registry. Workspace files override bundled ones with the same
//! `id`.

use std::time::Duration;

use tauri::{AppHandle, Runtime};

use crate::error::AppError;
use crate::models::ProfileSummary;
use crate::swarm::profile::ProfileSource;
use crate::swarm::{InvokeResult, ProfileRegistry, SubprocessTransport, Transport};

use super::workspace_agents_dir;

/// 60-second budget for `swarm:test_invoke`. WP §4 calls for this as
/// the default; the Windows AV cold-start risk noted in WP §"Notes"
/// motivates being generous.
const SWARM_INVOKE_TIMEOUT: Duration = Duration::from_secs(60);

/// Return every profile the registry knows about. Bundled defaults
/// always present (3 entries on a fresh install); workspace files
/// shadow bundled ones with the same `id`. Body and `source_path`
/// are stripped per `ProfileSummary`'s contract.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_profiles_list<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<ProfileSummary>, AppError> {
    let workspace_dir = workspace_agents_dir(&app)?;
    let registry =
        ProfileRegistry::load_from(workspace_dir.as_deref())?;

    let mut summaries: Vec<ProfileSummary> = registry
        .list()
        .into_iter()
        .map(|p| ProfileSummary {
            id: p.id.clone(),
            version: p.version.clone(),
            role: p.role.clone(),
            description: p.description.clone(),
            permission_mode: p.permission_mode,
            max_turns: p.max_turns,
            allowed_tools: p.allowed_tools.clone(),
            source: registry
                .source(&p.id)
                .unwrap_or(ProfileSource::Bundled)
                .as_str()
                .to_string(),
        })
        .collect();
    // Stable order so the UI's listing is deterministic.
    summaries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(summaries)
}

/// Spawn `claude` against the named profile, send `user_message`
/// once, return the parsed `result` event. Acceptance gate for
/// WP-W3-11 — proves the subprocess pipe is healthy end-to-end.
///
/// 60-second timeout absorbs Windows AV cold-start cost on first
/// spawn (per WP §"Notes / risks"). Subscription env is preserved
/// (no `ANTHROPIC_API_KEY` injected) per `binding::subscription_env`.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_test_invoke<R: Runtime>(
    app: AppHandle<R>,
    profile_id: String,
    user_message: String,
) -> Result<InvokeResult, AppError> {
    if profile_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "profileId must not be empty".into(),
        ));
    }
    if user_message.is_empty() {
        return Err(AppError::InvalidInput(
            "userMessage must not be empty".into(),
        ));
    }
    let workspace_dir = workspace_agents_dir(&app)?;
    let registry =
        ProfileRegistry::load_from(workspace_dir.as_deref())?;
    let profile = registry.get(&profile_id).ok_or_else(|| {
        AppError::NotFound(format!("swarm profile `{profile_id}`"))
    })?;
    let transport = SubprocessTransport::new();
    transport
        .invoke(&app, profile, &user_message, SWARM_INVOKE_TIMEOUT)
        .await
}

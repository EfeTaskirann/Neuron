//! `swarm:*` namespace — WP-W3-11 substrate command surface.
//!
//! Two commands:
//!
//! - `swarm:profiles_list()` → directory of bundled-default and
//!   workspace-override profiles, stripped of the persona body.
//! - `swarm:test_invoke(profileId, userMessage)` → spawn a one-shot
//!   `claude` subprocess against the named profile, send the user
//!   message, return the parsed `result` event.
//!
//! Both commands resolve the workspace-override dir from
//! `app_data_dir`'s `agents/` subdirectory and pass it (optionally)
//! into `ProfileRegistry::load_from` — bundled profiles are read
//! unconditionally via `include_dir!` inside the registry. Workspace
//! files override bundled ones with the same `id`.
//!
//! Phase 1 is one-shot only — `swarm:test_invoke` blocks until the
//! `result` event arrives. W3-12 introduces the streaming variant
//! that emits per-event Tauri events for the multi-pane UI.

use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::models::ProfileSummary;
use crate::swarm::profile::ProfileSource;
use crate::swarm::{InvokeResult, ProfileRegistry, SubprocessTransport};

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
    SubprocessTransport::invoke(
        &app,
        profile,
        &user_message,
        SWARM_INVOKE_TIMEOUT,
    )
    .await
}

/// Resolve `<app_data_dir>/agents`. Returns `None` (no error) when
/// the directory does not exist — workspace overrides are optional
/// per WP §2. Errors reaching `app_data_dir` itself are real (the
/// platform Tauri helper failed) and surface as `Internal`.
fn workspace_agents_dir<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<std::path::PathBuf>, AppError> {
    let base = app.path().app_data_dir().map_err(|e| {
        AppError::Internal(format!("app_data_dir resolution: {e}"))
    })?;
    let dir = base.join("agents");
    if dir.is_dir() {
        Ok(Some(dir))
    } else {
        Ok(None)
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;

    /// Acceptance: on a fresh install (no `<app_data_dir>/agents/`),
    /// `swarm:profiles_list` returns exactly the three bundled
    /// profiles in deterministic order.
    #[tokio::test]
    async fn profiles_list_returns_three_bundled() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let summaries = swarm_profiles_list(app.handle().clone())
            .await
            .expect("ok");
        let ids: Vec<&str> =
            summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["backend-builder", "planner", "scout"]);
        for s in &summaries {
            assert_eq!(
                s.source, "bundled",
                "fresh install: every profile must be bundled"
            );
        }
    }

    /// `swarm:test_invoke` rejects unknown profile ids before
    /// spawning anything.
    #[tokio::test]
    async fn test_invoke_unknown_profile_returns_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "no-such-profile".into(),
            "hello".into(),
        )
        .await
        .expect_err("unknown profile rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Empty profile id is `invalid_input`, not `not_found`.
    #[tokio::test]
    async fn test_invoke_empty_profile_id_rejected() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "".into(),
            "hello".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Empty user message is `invalid_input`.
    #[tokio::test]
    async fn test_invoke_empty_message_rejected() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "scout".into(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }
}

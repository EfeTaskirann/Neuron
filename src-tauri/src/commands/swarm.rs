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

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::models::ProfileSummary;
use crate::swarm::coordinator::{JobDetail, JobState, JobSummary};
use crate::swarm::profile::ProfileSource;
use crate::swarm::{
    CoordinatorFsm, InvokeResult, JobOutcome, JobRegistry, ProfileRegistry,
    SubprocessTransport, Transport,
};

/// Default page size for `swarm:list_jobs`. WP-W3-12b §4.
const SWARM_LIST_JOBS_DEFAULT_LIMIT: u32 = 50;
/// Hard cap to prevent runaway queries (full pagination is W3-14).
const SWARM_LIST_JOBS_MAX_LIMIT: u32 = 200;

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

/// Default per-stage budget for `swarm:run_job`. Matches
/// `SWARM_INVOKE_TIMEOUT` (60s, the W3-11 default) and can be
/// overridden per-process via `NEURON_SWARM_STAGE_TIMEOUT_SEC`.
const SWARM_STAGE_TIMEOUT_DEFAULT: Duration = Duration::from_secs(60);

/// Resolve the per-stage timeout. WP-W3-12a §3 calls for a
/// `NEURON_SWARM_STAGE_TIMEOUT_SEC` env override; non-numeric or
/// zero values fall back to the default with a structured warning so
/// a typo isn't silently ignored.
fn stage_timeout() -> Duration {
    const ENV: &str = "NEURON_SWARM_STAGE_TIMEOUT_SEC";
    match std::env::var(ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>() {
            Ok(0) => {
                tracing::warn!(
                    %ENV,
                    "value `0` is not a valid stage timeout; falling back to default"
                );
                SWARM_STAGE_TIMEOUT_DEFAULT
            }
            Ok(secs) => Duration::from_secs(secs),
            Err(e) => {
                tracing::warn!(
                    %ENV,
                    raw = %raw,
                    error = %e,
                    "stage timeout override is not a non-negative integer; using default"
                );
                SWARM_STAGE_TIMEOUT_DEFAULT
            }
        },
        _ => SWARM_STAGE_TIMEOUT_DEFAULT,
    }
}

/// Drive a 3-stage swarm job to completion (WP-W3-12a §4).
///
/// Walks `scout` → `planner` → `backend-builder` against the
/// substrate from W3-11, returning the aggregated `JobOutcome`. The
/// IPC blocks until the FSM finishes (Done / Failed). Two calls with
/// the same `workspace_id` serialize — the second returns
/// `AppError::WorkspaceBusy`. Two calls with different `workspace_id`s
/// run in parallel.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_run_job<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    goal: String,
) -> Result<JobOutcome, AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    if goal.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "goal must not be empty".into(),
        ));
    }

    let workspace_dir = workspace_agents_dir(&app)?;
    let profiles = std::sync::Arc::new(
        ProfileRegistry::load_from(workspace_dir.as_deref())?,
    );
    let registry = app
        .try_state::<std::sync::Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let transport = SubprocessTransport::new();
    let fsm = CoordinatorFsm::new(profiles, transport, registry, stage_timeout());
    fsm.run_job(&app, workspace_id, goal).await
}

/// Signal cancellation for an in-flight swarm job (WP-W3-12c §4).
///
/// Looks up `job_id` in the `JobRegistry`. Returns:
///
/// - `Ok(())` if the job was in-flight and the cancel signal was
///   delivered. The FSM observes the signal at the next `select!`
///   point, emits `Cancelled` then `Finished`, and finalizes the
///   job as `Failed` with `last_error = "cancelled by user"`.
/// - `Err(AppError::NotFound)` if no job with the given id exists
///   in the registry.
/// - `Err(AppError::Conflict)` if the job is already terminal
///   (`Done`/`Failed`) — including a previous cancel that has
///   already finalized.
///
/// Idempotency: a second cancel against the same in-flight job
/// either returns `Ok(())` (signal sent again, FSM ignores it
/// once finalized) or `Err(Conflict)` if the FSM has already
/// removed the cancel notify on its tail. The race is benign;
/// callers should treat both as "cancel acknowledged".
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_cancel_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<(), AppError> {
    if job_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "jobId must not be empty".into(),
        ));
    }

    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();

    // 1. Look up the job. Unknown id → NotFound.
    let job = registry.get(&job_id).ok_or_else(|| {
        AppError::NotFound(format!("swarm job `{job_id}`"))
    })?;
    // 2. Terminal jobs cannot be cancelled. The FSM has already
    //    removed the cancel notify by the time `state` flips, so
    //    we surface the precondition explicitly.
    if matches!(job.state, JobState::Done | JobState::Failed) {
        return Err(AppError::Conflict(format!(
            "swarm job `{job_id}` is already terminal ({:?})",
            job.state
        )));
    }
    // 3. Signal cancel. NotFound here means the job finalized
    //    between step 1 and step 3 — race; treat as Conflict so
    //    the caller sees a single "already terminal" semantic
    //    regardless of which side of the race they hit.
    match registry.signal_cancel(&job_id) {
        Ok(()) => Ok(()),
        Err(AppError::NotFound(_)) => Err(AppError::Conflict(format!(
            "swarm job `{job_id}` is already terminal"
        ))),
        Err(other) => Err(other),
    }
}

/// List recent swarm jobs from persisted history (WP-W3-12b §4).
///
/// `workspace_id` filters on the indexed `swarm_jobs.workspace_id`
/// column when supplied. `limit` defaults to 50 and is hard-capped
/// at 200 — full pagination is W3-14's UI surface.
///
/// Returns an empty `Vec` (not `Err`) when the registry is in-memory
/// only (no pool wired) — that's the test harness path; production
/// always has the pool.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_list_jobs<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<JobSummary>, AppError> {
    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let effective_limit = limit
        .unwrap_or(SWARM_LIST_JOBS_DEFAULT_LIMIT)
        .min(SWARM_LIST_JOBS_MAX_LIMIT);
    registry
        .list_jobs(workspace_id.as_deref(), effective_limit)
        .await
}

/// Fetch the full detail (job + every persisted stage) for one job
/// (WP-W3-12b §4). Unknown ids surface as `AppError::NotFound`.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_get_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<JobDetail, AppError> {
    if job_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "jobId must not be empty".into(),
        ));
    }
    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    match registry.get_job_detail(&job_id).await? {
        Some(detail) => Ok(detail),
        None => Err(AppError::NotFound(format!("swarm job {job_id}"))),
    }
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

    // ---------------------------------------------------------------- //
    // WP-W3-12c — swarm:cancel_job tests                                //
    // ---------------------------------------------------------------- //

    /// Cancel against a job_id that the registry has never seen
    /// surfaces `NotFound`.
    #[tokio::test]
    async fn cancel_unknown_job_id_returns_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        app.manage(registry);
        let err = swarm_cancel_job(app.handle().clone(), "j-nonexistent".into())
            .await
            .expect_err("unknown rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Cancel against an empty job_id surfaces `InvalidInput`.
    #[tokio::test]
    async fn cancel_empty_job_id_returns_invalid_input() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        app.manage(registry);
        let err = swarm_cancel_job(app.handle().clone(), "".into())
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Cancel against a job that has already completed (Done /
    /// Failed) surfaces `Conflict`.
    #[tokio::test]
    async fn cancel_already_terminal_returns_conflict() {
        use crate::swarm::coordinator::Job;
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        // Insert a terminal job by hand — bypasses the FSM but
        // exercises the same registry surface the real FSM writes.
        let job = Job {
            id: "j-done".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Done,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
        };
        registry
            .try_acquire_workspace("ws-done", job)
            .await
            .expect("acquire");
        app.manage(registry);
        let err = swarm_cancel_job(app.handle().clone(), "j-done".into())
            .await
            .expect_err("terminal rejected");
        assert_eq!(err.kind(), "conflict");
    }

    /// Cancel against a Failed job also surfaces `Conflict`
    /// (terminal == Done OR Failed; cancelled jobs ride the Failed
    /// path).
    #[tokio::test]
    async fn cancel_failed_job_returns_conflict() {
        use crate::swarm::coordinator::Job;
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let job = Job {
            id: "j-failed".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Failed,
            retry_count: 0,
            stages: Vec::new(),
            last_error: Some("boom".into()),
        };
        registry
            .try_acquire_workspace("ws-failed", job)
            .await
            .expect("acquire");
        app.manage(registry);
        let err = swarm_cancel_job(app.handle().clone(), "j-failed".into())
            .await
            .expect_err("terminal rejected");
        assert_eq!(err.kind(), "conflict");
    }

    /// In-flight job (state is one of Init/Scout/Plan/Build) but
    /// no cancel notify registered → `Conflict` (race: the FSM
    /// removed the notify on its tail before the IPC reached
    /// `signal_cancel`). The command translates `NotFound` from
    /// `signal_cancel` into `Conflict` on this branch so the
    /// caller sees a single "already terminal" semantic.
    #[tokio::test]
    async fn cancel_in_flight_without_notify_returns_conflict() {
        use crate::swarm::coordinator::Job;
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let job = Job {
            id: "j-mid".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Build,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
        };
        registry
            .try_acquire_workspace("ws-mid", job)
            .await
            .expect("acquire");
        // Note: no register_cancel call — simulates the race where
        // the FSM tail has already unregistered.
        app.manage(registry);
        let err = swarm_cancel_job(app.handle().clone(), "j-mid".into())
            .await
            .expect_err("race rejected");
        assert_eq!(err.kind(), "conflict");
    }

    /// In-flight job with cancel notify registered → cancel
    /// signals successfully. We register the notify by hand so the
    /// test doesn't need to spin up the full FSM.
    #[tokio::test]
    async fn cancel_in_flight_with_notify_returns_ok() {
        use crate::swarm::coordinator::Job;
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let job = Job {
            id: "j-live".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Scout,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
        };
        registry
            .try_acquire_workspace("ws-live", job)
            .await
            .expect("acquire");
        let notify = Arc::new(tokio::sync::Notify::new());
        registry
            .register_cancel("j-live", Arc::clone(&notify))
            .expect("register");
        app.manage(registry);

        // Subscribe to the notify *before* we signal so we can
        // assert the cancel actually woke a waiter.
        let waiter = tokio::spawn(async move {
            notify.notified().await;
        });
        tokio::task::yield_now().await;

        swarm_cancel_job(app.handle().clone(), "j-live".into())
            .await
            .expect("ok");
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter wakes within 1s")
            .expect("waiter task panicked");
    }

    /// Double-cancel against the same in-flight job — the second
    /// call must surface `Conflict` or `NotFound` (race-dependent
    /// on whether the FSM tail has unregistered the notify yet).
    /// We hand-build the registry state without an FSM so the
    /// race is deterministic: after the first signal, we manually
    /// unregister the cancel notify (simulating the FSM tail) and
    /// flip the job to Failed before issuing the second signal.
    #[tokio::test]
    async fn cancel_double_signal_second_returns_error() {
        use crate::swarm::coordinator::Job;
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        let job = Job {
            id: "j-double".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Scout,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
        };
        registry
            .try_acquire_workspace("ws-double", job)
            .await
            .expect("acquire");
        let notify = Arc::new(tokio::sync::Notify::new());
        registry
            .register_cancel("j-double", Arc::clone(&notify))
            .expect("register");
        app.manage(Arc::clone(&registry));

        // First cancel — succeeds.
        swarm_cancel_job(app.handle().clone(), "j-double".into())
            .await
            .expect("first cancel ok");

        // Simulate the FSM tail: flip to Failed and unregister
        // the notify. Order matches what the real FSM does in
        // `finalize_cancelled` + the `CancelGuard` Drop.
        registry
            .update("j-double", |j| {
                j.state = JobState::Failed;
                j.last_error = Some("cancelled by user".into());
            })
            .await
            .expect("update");
        registry.unregister_cancel("j-double");

        // Second cancel — must fail. Conflict (terminal) is the
        // expected branch, but a NotFound from a different race
        // is also acceptable per the WP contract.
        let err = swarm_cancel_job(app.handle().clone(), "j-double".into())
            .await
            .expect_err("second cancel rejected");
        assert!(
            matches!(err, AppError::Conflict(_) | AppError::NotFound(_)),
            "second cancel must be Conflict or NotFound; got: {err:?}"
        );
    }

    /// `swarm_cancel_job` requires the JobRegistry in app state.
    /// Missing state surfaces `Internal`. Defensive; the real
    /// `lib.rs::setup` always registers the registry.
    #[tokio::test]
    async fn cancel_without_registry_state_returns_internal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // Intentionally do NOT manage(JobRegistry).
        let err = swarm_cancel_job(app.handle().clone(), "j-anything".into())
            .await
            .expect_err("no registry rejected");
        assert_eq!(err.kind(), "internal");
    }

    // ---------------------------------------------------------------- //
    // WP-W3-12b — swarm:list_jobs / swarm:get_job IPC tests             //
    // ---------------------------------------------------------------- //

    use crate::swarm::coordinator::Job;

    /// Seed `n` finished jobs into the pool via the registry, then
    /// invoke `swarm_list_jobs` and assert the wire shape.
    #[tokio::test]
    async fn swarm_list_jobs_command_returns_summaries() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry =
            Arc::new(JobRegistry::with_pool(pool.clone()));
        // Seed three jobs across one workspace.
        for i in 0..3 {
            let job = Job {
                id: format!("j-{i}"),
                goal: format!("goal {i}"),
                created_at_ms: i as i64,
                state: JobState::Init,
                retry_count: 0,
                stages: Vec::new(),
                last_error: None,
            };
            registry
                .try_acquire_workspace("ws-list", job)
                .await
                .expect("acquire");
            registry
                .update(&format!("j-{i}"), |j| {
                    j.state = JobState::Done;
                })
                .await
                .expect("flip done");
            registry
                .release_workspace("ws-list", &format!("j-{i}"))
                .await;
        }
        app.manage(registry);

        let summaries = swarm_list_jobs(
            app.handle().clone(),
            None,
            Some(50),
        )
        .await
        .expect("list ok");
        assert_eq!(summaries.len(), 3);
        // Ordered newest-first by created_at_ms.
        assert_eq!(summaries[0].id, "j-2");
        for s in &summaries {
            assert_eq!(s.workspace_id, "ws-list");
            assert_eq!(s.state, JobState::Done);
        }
    }

    /// `swarm_list_jobs` defaults `limit` to 50 when omitted; we
    /// verify the call shape rather than the cap by passing > 200.
    #[tokio::test]
    async fn swarm_list_jobs_caps_limit_at_200() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry =
            Arc::new(JobRegistry::with_pool(pool.clone()));
        app.manage(registry);
        // Empty result is still Ok with the bounded limit applied.
        let result = swarm_list_jobs(
            app.handle().clone(),
            None,
            Some(9999),
        )
        .await
        .expect("list ok");
        assert!(result.is_empty());
    }

    /// `swarm_get_job` returns the full detail for a known id.
    #[tokio::test]
    async fn swarm_get_job_command_returns_detail() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry =
            Arc::new(JobRegistry::with_pool(pool.clone()));
        let job = Job {
            id: "j-detail".into(),
            goal: "detail goal".into(),
            created_at_ms: 999,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
        };
        registry
            .try_acquire_workspace("ws-detail", job)
            .await
            .expect("acquire");
        registry
            .update("j-detail", |j| {
                j.state = JobState::Done;
            })
            .await
            .expect("update");
        app.manage(registry);

        let detail = swarm_get_job(app.handle().clone(), "j-detail".into())
            .await
            .expect("get ok");
        assert_eq!(detail.id, "j-detail");
        assert_eq!(detail.workspace_id, "ws-detail");
        assert_eq!(detail.goal, "detail goal");
        assert_eq!(detail.state, JobState::Done);
    }

    /// Unknown job id at the IPC layer surfaces `NotFound`.
    #[tokio::test]
    async fn swarm_get_job_unknown_returns_not_found_error() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry =
            Arc::new(JobRegistry::with_pool(pool.clone()));
        app.manage(registry);
        let err = swarm_get_job(app.handle().clone(), "j-nope".into())
            .await
            .expect_err("unknown rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Empty job id surfaces `InvalidInput` before touching the DB.
    #[tokio::test]
    async fn swarm_get_job_empty_id_rejected() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry =
            Arc::new(JobRegistry::with_pool(pool.clone()));
        app.manage(registry);
        let err = swarm_get_job(app.handle().clone(), "".into())
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_list_jobs` requires the registry in app state.
    #[tokio::test]
    async fn swarm_list_jobs_without_registry_returns_internal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_list_jobs(app.handle().clone(), None, None)
            .await
            .expect_err("missing registry");
        assert_eq!(err.kind(), "internal");
    }

    /// `swarm_get_job` requires the registry in app state.
    #[tokio::test]
    async fn swarm_get_job_without_registry_returns_internal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_get_job(app.handle().clone(), "j".into())
            .await
            .expect_err("missing registry");
        assert_eq!(err.kind(), "internal");
    }
}

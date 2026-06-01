//! `swarm:run_job` — drive the Coordinator brain dispatch loop
//! (WP-W5-06, originally W5-03 `swarm:run_job_v2`).
//!
//! Also hosts the test-only [`swarm_run_job_with_invoker`] entry
//! point that the brain IPC tests use to swap in a scripted
//! `CoordinatorInvoker` mock — kept here (not in tests) so it
//! shares [`finalise_run_job`] with the real IPC.

use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
// `JobRegistry` is referenced via the fully-qualified
// `crate::swarm::JobRegistry` path inside `finalise_run_job`'s
// `&crate::swarm::JobRegistry` argument; importing the alias would
// be a redundant unused import.
use crate::swarm::{JobOutcome, MailboxBus, SwarmAgentRegistry};

/// IDs of the specialist agents the brain may dispatch to. These get
/// `ensure_dispatcher` called on them up front so the bus picks up
/// every dispatch the brain emits without a per-target spawn race.
/// `coordinator` is intentionally absent — the brain talks to its
/// own session through `CoordinatorInvoker`, not through a
/// dispatcher.
const SPECIALIST_AGENT_IDS: &[&str] = &[
    "scout",
    "planner",
    "backend-builder",
    "backend-reviewer",
    "frontend-builder",
    "frontend-reviewer",
    "integration-tester",
];

/// Drive the Coordinator brain dispatch loop to completion (WP-W5-06,
/// previously `swarm:run_job_v2` from W5-03).
///
/// Lifecycle:
/// 1. Mint a `j-<ULID>` if not preset; acquire the workspace lock
///    via the existing `JobRegistry::try_acquire_workspace`.
/// 2. Ensure a `MailboxAgentDispatcher` exists for every specialist
///    agent so dispatches the brain emits land on a real receiver
///    immediately.
/// 3. Spawn the brain on `CoordinatorBrain::run` with the workspace's
///    `MailboxBus` and the production `CoordinatorInvoker`.
/// 4. Await the brain's `BrainRunResult` and build a `JobOutcome`
///    from the W5-04 projector's `swarm_jobs` / `swarm_stages`
///    rows.
/// 5. Release the workspace lock.
///
/// Returns the projector-built `JobOutcome` on success / failure
/// paths. `final_state` maps from brain outcome via the projector:
///   - terminal `JobFinished` event with `outcome="done"` →
///     `JobState::Done`
///   - terminal `JobFinished` with anything else → `JobState::Failed`
///   - terminal `JobCancel` → `JobState::Failed` with `last_error =
///     "cancelled by user"`.
///
/// W5-06 — frontend signature unchanged from the legacy FSM IPC:
/// `(workspaceId, goal) -> JobOutcome`. Callers
/// (`useRunSwarmJob`) keep working without code change.
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

    let job_registry = app
        .try_state::<Arc<crate::swarm::JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let agent_registry = app
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
    let bus = app
        .try_state::<Arc<MailboxBus>>()
        .ok_or_else(|| {
            AppError::Internal(
                "MailboxBus missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();

    // Mint job + acquire workspace lock. Reuse the existing
    // `try_acquire_workspace` for compatibility (W5-05 migrates
    // this off the registry).
    let job_id = format!("j-{}", ulid::Ulid::new());
    let now_ms = crate::time::now_millis();
    let started_at_ms = now_ms;
    let job = crate::swarm::Job {
        id: job_id.clone(),
        goal: goal.clone(),
        created_at_ms: now_ms,
        state: crate::swarm::JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        // W5-04: brain-driven jobs land with `source='brain'` so
        // the projector's row writes (and any subsequent reads via
        // `swarm:get_job` / `swarm:list_jobs`) carry the right
        // discriminator. The FSM path (`swarm:run_job`) keeps the
        // default `'fsm'` value.
        source: "brain".into(),
    };
    job_registry
        .try_acquire_workspace(&workspace_id, job)
        .await?;

    // WP-W5-04 — ensure the workspace's `JobProjector` is up.
    // Idempotent; the registry's RwLock fast path returns
    // immediately when the projector already exists. Spawning the
    // projector BEFORE wiring the dispatchers (and therefore
    // before any brain emit) keeps the broadcast subscriber
    // ordering: projector subscribed first, then dispatchers,
    // then the brain emits JobStarted.
    if let Some(projector_registry) = app
        .try_state::<Arc<crate::swarm::JobProjectorRegistry>>()
    {
        let projector_registry = projector_registry.inner().clone();
        let pool_for_projector = app
            .state::<crate::db::DbPool>()
            .inner()
            .clone();
        projector_registry
            .ensure_for_workspace(
                &app,
                &workspace_id,
                std::sync::Arc::clone(&bus),
                pool_for_projector,
            )
            .await;
    }

    // Ensure a MailboxAgentDispatcher is wired up for each specialist
    // so the brain's dispatches don't race against late-spawning
    // dispatchers. Idempotent — second call is a no-op.
    for agent_id in SPECIALIST_AGENT_IDS {
        agent_registry
            .ensure_dispatcher(&app, &workspace_id, agent_id, &bus)
            .await;
    }

    // Build the production CoordinatorInvoker and run the brain
    // inline (not on a spawned task — the IPC call is the await
    // boundary; cancellation comes through the workspace's mailbox
    // event-bus and signal_cancel, both of which are independent of
    // this task's join handle).
    let invoker = std::sync::Arc::new(
        crate::swarm::SwarmRegistryCoordinatorInvoker::new(
            app.clone(),
            std::sync::Arc::clone(&agent_registry),
        ),
    );
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    // Best-effort: register the cancel notify so `swarm:cancel_job`
    // can target the v2 path too. The W5-05 cancel migration makes
    // this canonical.
    let _ = job_registry.register_cancel(&job_id, std::sync::Arc::clone(&cancel));

    let brain_result = crate::swarm::CoordinatorBrain::run(
        app.clone(),
        workspace_id.clone(),
        job_id.clone(),
        goal.clone(),
        invoker,
        std::sync::Arc::clone(&bus),
        cancel,
    )
    .await;
    finalise_run_job(
        &app,
        &job_registry,
        &workspace_id,
        &job_id,
        started_at_ms,
        brain_result,
    )
    .await
}

/// Internal: shared finalisation logic between `swarm_run_job` and
/// the test-only entry point. Releases the workspace lock,
/// unregisters the cancel notify, updates the in-memory job state,
/// and asks the [`JobProjector`] to build the canonical
/// [`JobOutcome`] from the persisted event log.
///
/// W5-04: defers to `JobProjector::build_outcome` which walks the
/// bus's SQL-persisted event log and returns the fully-shaped
/// outcome.
async fn finalise_run_job<R: Runtime>(
    app: &AppHandle<R>,
    job_registry: &crate::swarm::JobRegistry,
    workspace_id: &str,
    job_id: &str,
    _started_at_ms: i64,
    brain_result: Result<crate::swarm::BrainRunResult, AppError>,
) -> Result<JobOutcome, AppError> {
    job_registry.unregister_cancel(job_id);
    job_registry
        .release_workspace(workspace_id, job_id)
        .await;
    match brain_result {
        Ok(_result) => {
            // Pull the bus from app state so `build_outcome` can
            // walk the event log. Fall back to a stub outcome only
            // if the bus is missing (defensive — `lib.rs::setup`
            // always installs it).
            let bus_state = app
                .try_state::<Arc<crate::swarm::MailboxBus>>();
            let pool_state = app.try_state::<crate::db::DbPool>();
            let outcome = match (bus_state, pool_state) {
                (Some(bus), Some(pool)) => {
                    let bus = bus.inner().clone();
                    let pool = pool.inner().clone();
                    crate::swarm::JobProjector::build_outcome(
                        &bus, &pool, job_id,
                    )
                    .await?
                }
                _ => {
                    // Defensive — should never happen in production
                    // (lib.rs::setup wires both). Surface a tame
                    // error so the caller sees a typed failure.
                    return Err(AppError::Internal(
                        "swarm:run_job: MailboxBus or DbPool missing \
                         from app state — cannot build JobOutcome"
                            .into(),
                    ));
                }
            };
            // Mirror the projector's terminal state into the
            // in-memory JobRegistry so `swarm:cancel_job` / future
            // status queries through the registry see the latest
            // shape.
            let final_state = outcome.final_state;
            let last_error_clone = outcome.last_error.clone();
            let _ = job_registry
                .update(job_id, |job| {
                    job.state = final_state;
                    job.last_error = last_error_clone.clone();
                })
                .await;
            Ok(outcome)
        }
        Err(err) => {
            let _ = job_registry
                .update(job_id, |job| {
                    job.state = crate::swarm::JobState::Failed;
                    job.last_error = Some(err.message().to_string());
                })
                .await;
            Err(err)
        }
    }
}

/// Test-only entry point: run the brain with a caller-provided
/// invoker (typically a mock that returns canned action sequences).
/// Mirrors `swarm_run_job`'s lifecycle so the tests can exercise
/// the same lock + finalisation path the IPC takes — they only swap
/// out the LLM-spawning piece.
///
/// `spawn_dispatchers` toggles whether the real
/// `MailboxAgentDispatcher`s are wired up. Tests that mock the
/// brain inline (and emit AgentResults from a helper task) pass
/// `false` so the real dispatchers don't race the helper to invoke
/// `claude`.
#[cfg(test)]
pub(crate) async fn swarm_run_job_with_invoker<R, I>(
    app: AppHandle<R>,
    workspace_id: String,
    goal: String,
    invoker: std::sync::Arc<I>,
    max_dispatches: u32,
    spawn_dispatchers: bool,
) -> Result<JobOutcome, AppError>
where
    R: Runtime,
    I: crate::swarm::CoordinatorInvoker,
{
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
    let job_registry = app
        .try_state::<Arc<crate::swarm::JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let agent_registry = app
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
    let bus = app
        .try_state::<Arc<MailboxBus>>()
        .ok_or_else(|| {
            AppError::Internal(
                "MailboxBus missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();

    let job_id = format!("j-{}", ulid::Ulid::new());
    let now_ms = crate::time::now_millis();
    let started_at_ms = now_ms;
    let job = crate::swarm::Job {
        id: job_id.clone(),
        goal: goal.clone(),
        created_at_ms: now_ms,
        state: crate::swarm::JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        // W5-04 — brain-driven path, see swarm_run_job above.
        source: "brain".into(),
    };
    job_registry
        .try_acquire_workspace(&workspace_id, job)
        .await?;
    if spawn_dispatchers {
        for agent_id in SPECIALIST_AGENT_IDS {
            agent_registry
                .ensure_dispatcher(&app, &workspace_id, agent_id, &bus)
                .await;
        }
    }
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let _ = job_registry
        .register_cancel(&job_id, std::sync::Arc::clone(&cancel));
    let brain_result = crate::swarm::CoordinatorBrain::run_with_max(
        app.clone(),
        workspace_id.clone(),
        job_id.clone(),
        goal,
        invoker,
        std::sync::Arc::clone(&bus),
        cancel,
        max_dispatches,
    )
    .await;
    finalise_run_job(
        &app,
        &job_registry,
        &workspace_id,
        &job_id,
        started_at_ms,
        brain_result,
    )
    .await
}

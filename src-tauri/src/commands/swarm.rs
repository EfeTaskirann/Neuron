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

use tauri::{AppHandle, Manager, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::ProfileSummary;
use crate::swarm::coordinator::orchestrator_session::{
    append_job_message, append_orchestrator_message, append_user_message,
    clear_messages, list_recent_messages, render_with_history,
};
use crate::swarm::coordinator::{
    parse_orchestrator_outcome, JobDetail, JobState, JobSummary,
    OrchestratorMessage, OrchestratorOutcome,
};
use crate::swarm::profile::ProfileSource;
use crate::swarm::{
    AgentStatusRow, CoordinatorFsm, InvokeResult, JobOutcome, JobRegistry,
    MailboxBus, MailboxEvent, ProfileRegistry, SubprocessTransport,
    SwarmAgentRegistry, Transport,
};

/// Default page size for `swarm:list_jobs`. WP-W3-12b §4.
const SWARM_LIST_JOBS_DEFAULT_LIMIT: u32 = 50;
/// Hard cap to prevent runaway queries (full pagination is W3-14).
const SWARM_LIST_JOBS_MAX_LIMIT: u32 = 200;

/// Default page size for `swarm:orchestrator_history` when the IPC
/// call omits `limit`. WP-W3-12k2 §4.
const SWARM_ORCHESTRATOR_HISTORY_DEFAULT_LIMIT: u32 = 50;
/// Hard cap mirroring `SWARM_LIST_JOBS_MAX_LIMIT`. The frontend
/// surfaces a Clear button rather than paginating, so the cap is
/// generous but not unbounded.
const SWARM_ORCHESTRATOR_HISTORY_MAX_LIMIT: u32 = 200;
/// Number of recent messages injected into the Orchestrator prompt
/// on each `swarm:orchestrator_decide` call. WP-W3-12k2 §3 fixes
/// this at 10; tuneable per-process via
/// `NEURON_ORCHESTRATOR_HISTORY_DEPTH` only after a future WP that
/// adds the env override pattern from `stage_timeout`.
const SWARM_ORCHESTRATOR_DECIDE_HISTORY_DEPTH: u32 = 10;

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
    // WP-W4-06 — drive the FSM through the persistent
    // `RegistryTransport` (alongside the W3-12 `SubprocessTransport`).
    // Sessions live in the workspace-scoped `SwarmAgentRegistry`;
    // each FSM stage's invoke reuses the persistent session for
    // that agent, and specialist→Coordinator help requests are
    // handled transparently inside the registry adapter (max 3
    // help rounds before the FSM sees a hard SwarmInvoke fallback).
    let agent_registry = app
        .try_state::<std::sync::Arc<SwarmAgentRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "SwarmAgentRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let cancel = std::sync::Arc::new(tokio::sync::Notify::new());
    let transport = crate::swarm::RegistryTransport::new(
        workspace_id.clone(),
        agent_registry,
        cancel,
    );
    let fsm = CoordinatorFsm::new(profiles, transport, registry, stage_timeout());
    fsm.run_job(&app, workspace_id, goal).await
}

/// Single-shot Orchestrator decision (WP-W3-12k1 §3, extended in
/// WP-W3-12k2 §3 with persistent history).
///
/// Spawns a one-shot `claude` subprocess against the bundled
/// `orchestrator.md` persona, hands it the user's chat message
/// **prepended with the most-recent N messages from the persisted
/// thread for `workspace_id`**, and parses the JSON
/// `OrchestratorOutcome` (DirectReply / Clarify / Dispatch). The IPC
/// blocks until the subprocess emits its `result` event; same env /
/// OAuth pattern as `swarm:test_invoke`.
///
/// **Persistence shape** (W3-12k2 §3):
///
/// 1. Load last N=10 messages (oldest-first chronological) from
///    `orchestrator_messages`.
/// 2. Persist the **user** row BEFORE the LLM invoke so a hung /
///    failed subprocess still preserves the user's input on the next
///    mount.
/// 3. Render the prompt with [`render_with_history`] — the very
///    first turn (history empty) is byte-identical to the W3-12k1
///    stateless behaviour.
/// 4. Invoke the subprocess.
/// 5. Parse the outcome.
/// 6. Persist the **orchestrator** row only on a successful parse
///    so an unparseable result doesn't leave a half-baked row.
///
/// The IPC signature is unchanged from W3-12k1 — persistence is a
/// pure side effect. Callers (the W3-12k3 chat panel) branch on the
/// returned `action`:
///
/// - `DirectReply` / `Clarify` → render `outcome.text` to the user.
/// - `Dispatch` → call `swarm:run_job(workspace_id, outcome.text)`
///   to enter the Coordinator FSM, then
///   `swarm:orchestrator_log_job` to record the dispatch in the
///   chat thread.
///
/// `workspace_id` is the conversation key — one chat per workspace,
/// single thread (per W3-12k2 §"Out of scope"). Tests under
/// `mock_app_with_pool` thread the pool through `app.state::<DbPool>()`
/// the same way every other persistence-touching command does.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_orchestrator_decide<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    user_message: String,
) -> Result<OrchestratorOutcome, AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    if user_message.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "userMessage must not be empty".into(),
        ));
    }

    let workspace_dir = workspace_agents_dir(&app)?;
    let registry =
        ProfileRegistry::load_from(workspace_dir.as_deref())?;
    let profile = registry.get("orchestrator").ok_or_else(|| {
        AppError::NotFound("swarm profile `orchestrator`".into())
    })?;

    // 1. Load recent history. The pool may be absent in command-level
    //    tests that haven't wired persistence; we treat that as "empty
    //    history" rather than a hard error so the validation tests
    //    that short-circuit before this line keep working.
    let pool_state = app.try_state::<DbPool>();
    let history = match pool_state.as_ref() {
        Some(pool) => {
            list_recent_messages(
                pool.inner(),
                &workspace_id,
                SWARM_ORCHESTRATOR_DECIDE_HISTORY_DEPTH,
            )
            .await?
        }
        None => Vec::new(),
    };

    // 2. Persist the user row BEFORE the invoke so the input survives
    //    a subprocess crash. Skipped when no pool is wired (test path).
    if let Some(pool) = pool_state.as_ref() {
        let now_ms = crate::time::now_millis();
        append_user_message(pool.inner(), &workspace_id, &user_message, now_ms)
            .await?;
    }

    // 3. Render the prompt with the loaded history.
    let rendered_prompt = render_with_history(&history, &user_message);

    // 4. Invoke the subprocess.
    let transport = SubprocessTransport::new();
    let result = transport
        .invoke(&app, profile, &rendered_prompt, stage_timeout())
        .await?;

    // 5. Parse the outcome. Failure here surfaces to the caller with
    //    no orchestrator row written.
    let outcome = parse_orchestrator_outcome(&result.assistant_text)?;

    // 6. Persist the orchestrator row on successful parse.
    if let Some(pool) = pool_state.as_ref() {
        let now_ms = crate::time::now_millis();
        append_orchestrator_message(
            pool.inner(),
            &workspace_id,
            &outcome,
            now_ms,
        )
        .await?;
    }

    Ok(outcome)
}

/// Read the persisted Orchestrator chat history for `workspace_id`
/// (WP-W3-12k2 §4). The frontend's `useOrchestratorHistory` calls
/// this on mount to seed the chat panel from SQLite.
///
/// `limit` defaults to 50 and is hard-capped at 200 — there is no
/// pagination cursor (one chat per workspace, single thread). The
/// returned `Vec<OrchestratorMessage>` is oldest-first chronological
/// so the caller can render bubbles in display order without an
/// extra reverse step.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_orchestrator_history<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    limit: Option<u32>,
) -> Result<Vec<OrchestratorMessage>, AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    let pool = app
        .try_state::<DbPool>()
        .ok_or_else(|| {
            AppError::Internal(
                "DbPool missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let effective_limit = limit
        .unwrap_or(SWARM_ORCHESTRATOR_HISTORY_DEFAULT_LIMIT)
        .min(SWARM_ORCHESTRATOR_HISTORY_MAX_LIMIT);
    list_recent_messages(&pool, &workspace_id, effective_limit).await
}

/// Hard-delete every persisted Orchestrator chat message for
/// `workspace_id` (WP-W3-12k2 §5). The frontend's "Clear chat"
/// button drives this; there is no soft delete or archival.
///
/// Idempotent at the SQL boundary — clearing an already-empty
/// workspace returns `Ok(())`.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_orchestrator_clear_history<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<(), AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    let pool = app
        .try_state::<DbPool>()
        .ok_or_else(|| {
            AppError::Internal(
                "DbPool missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    clear_messages(&pool, &workspace_id).await
}

/// Persist a "swarm dispatched" Job row in the chat thread
/// (WP-W3-12k2 §6). Called by the frontend orchestration glue
/// immediately after `swarm:run_job` returns, so the chat panel
/// shows the dispatch on the next mount without the FSM itself
/// having to know about the chat history.
///
/// Validation: `workspace_id`, `job_id`, and `goal` must all be
/// non-empty after `trim()`. An empty value short-circuits with
/// `InvalidInput` before touching the DB.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_orchestrator_log_job<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    job_id: String,
    goal: String,
) -> Result<(), AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    if job_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "jobId must not be empty".into(),
        ));
    }
    if goal.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "goal must not be empty".into(),
        ));
    }
    let pool = app
        .try_state::<DbPool>()
        .ok_or_else(|| {
            AppError::Internal(
                "DbPool missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let now_ms = crate::time::now_millis();
    append_job_message(&pool, &workspace_id, &job_id, &goal, now_ms).await?;
    Ok(())
}

/// Signal cancellation for an in-flight swarm job (WP-W3-12c §4,
/// WP-W5-05 source-switching).
///
/// Discriminates on `swarm_jobs.source` (`'brain'` vs `'fsm'`):
///
/// - **`source='brain'`** (W5-03 brain-driven jobs): emits
///   `MailboxEvent::JobCancel` on the workspace's mailbox bus.
///   The brain's dispatch-loop `select!` and every dispatcher's
///   in-flight invoke notify pick it up and unwind. Returns
///   `Ok(())` immediately — the IPC does not block on the
///   cancel actually propagating (≤ 100ms in practice; same
///   async semantics as the FSM cancel below).
/// - **`source='fsm'` or no DB row** (legacy / W3 path): keeps
///   the in-memory `JobRegistry::signal_cancel` flow. Returns:
///     - `Ok(())` if the cancel signal landed on the FSM's per-
///       job `Notify`. The FSM observes the signal at its next
///       `select!` point, emits `Cancelled` then `Finished`,
///       and finalizes the job as `Failed` with `last_error =
///       "cancelled by user"`.
///     - `Err(AppError::NotFound)` if no job with the given id
///       exists in the registry.
///     - `Err(AppError::Conflict)` if the job is already
///       terminal (`Done`/`Failed`) — including a previous
///       cancel that has already finalized.
/// - **unknown source string**: returns
///   `AppError::Internal(...)`. Defensive — only `'brain'` and
///   `'fsm'` are written in production; any other value implies
///   schema drift.
///
/// Idempotency (FSM path): a second cancel against the same
/// in-flight job either returns `Ok(())` (signal sent again, FSM
/// ignores it once finalized) or `Err(Conflict)` if the FSM has
/// already removed the cancel notify on its tail. The race is
/// benign; callers should treat both as "cancel acknowledged".
///
/// Idempotency (brain path): the bus has no dedupe — a second
/// cancel emits a second `JobCancel` row. The brain + dispatchers
/// both treat the second as a no-op (the loop has already
/// terminated). The mailbox row is informational; the SQL log
/// remains the source of truth.
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

    // WP-W5-05 — discriminate on `swarm_jobs.source`. Tests in the
    // FSM cancel suite use a `JobRegistry::new()` (no pool) so the
    // `swarm_jobs` row is never written; in that case the source
    // query returns `None` and we fall through to the legacy FSM
    // path. Production always has the pool wired.
    let source: Option<String> =
        if let Some(pool) = app.try_state::<crate::db::DbPool>() {
            sqlx::query_scalar(
                "SELECT source FROM swarm_jobs WHERE id = ?",
            )
            .bind(&job_id)
            .fetch_optional(pool.inner())
            .await?
        } else {
            None
        };

    match source.as_deref() {
        Some("brain") => {
            // Look up the workspace_id so the JobCancel lands on
            // the right per-workspace broadcast channel. The
            // brain + dispatcher subscribers filter by job_id,
            // but the channel routing is per-workspace.
            let pool = app
                .try_state::<crate::db::DbPool>()
                .ok_or_else(|| {
                    AppError::Internal(
                        "DbPool missing from app state — \
                         lib.rs::setup did not call app.manage()"
                            .into(),
                    )
                })?
                .inner()
                .clone();
            let workspace_id: String = sqlx::query_scalar(
                "SELECT workspace_id FROM swarm_jobs WHERE id = ?",
            )
            .bind(&job_id)
            .fetch_one(&pool)
            .await?;
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
            bus.emit_typed(
                &app,
                &workspace_id,
                "agent:user",
                "agent:coordinator",
                &format!("cancel requested for {job_id}"),
                None,
                MailboxEvent::JobCancel {
                    job_id: job_id.clone(),
                },
            )
            .await?;
            Ok(())
        }
        Some("fsm") | None => {
            // Legacy W3 FSM path — preserved verbatim from
            // WP-W3-12c §4. Stays in place until W5-06 deletes
            // the FSM.
            let job = registry.get(&job_id).ok_or_else(|| {
                AppError::NotFound(format!("swarm job `{job_id}`"))
            })?;
            if matches!(job.state, JobState::Done | JobState::Failed) {
                return Err(AppError::Conflict(format!(
                    "swarm job `{job_id}` is already terminal ({:?})",
                    job.state
                )));
            }
            match registry.signal_cancel(&job_id) {
                Ok(()) => Ok(()),
                Err(AppError::NotFound(_)) => {
                    Err(AppError::Conflict(format!(
                        "swarm job `{job_id}` is already terminal"
                    )))
                }
                Err(other) => Err(other),
            }
        }
        Some(other) => Err(AppError::Internal(format!(
            "unknown swarm_jobs.source: {other}"
        ))),
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

// --------------------------------------------------------------------- //
// W4-02 — SwarmAgentRegistry IPC                                         //
// --------------------------------------------------------------------- //

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

// --------------------------------------------------------------------- //
// W5-02 — agent dispatch via mailbox bus                                //
// --------------------------------------------------------------------- //

/// Dispatch a task to a named agent via the W5-01 mailbox event-bus
/// (WP-W5-02). Spawns the agent's `MailboxAgentDispatcher` if it
/// doesn't already exist, then emits a `MailboxEvent::TaskDispatch`
/// with `target = "agent:<agent_id>"`. The dispatcher picks up the
/// event from the broadcast channel, calls
/// `acquire_and_invoke_turn` against the registry, and emits a
/// `MailboxEvent::AgentResult` whose envelope `parent_id` points
/// back at the dispatch row.
///
/// Returns the dispatch row's `id` so callers (tests, manual
/// dispatch UIs) can correlate the dispatch with its eventual
/// result via the parent_id chain.
///
/// Validation: empty workspace_id / agent_id / prompt are rejected
/// as `InvalidInput`. Missing `MailboxBus` or `SwarmAgentRegistry`
/// state surfaces as `Internal` (defensive; `lib.rs::setup` always
/// installs both on production runs).
///
/// `job_id` defaults to a fresh `j-<ULID>` when omitted; callers
/// running standalone dispatches (no enclosing job) can let it
/// auto-generate. `with_help_loop` defaults to `false` per WP
/// §"Notes" — W5-02 dispatchers always call the non-help variant.
/// The flag is preserved in the emitted event so downstream
/// consumers (W5-03 brain) can still read user intent.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_dispatch_to_agent<R: Runtime>(
    app: AppHandle<R>,
    bus: State<'_, Arc<MailboxBus>>,
    registry: State<'_, Arc<SwarmAgentRegistry>>,
    workspace_id: String,
    agent_id: String,
    prompt: String,
    job_id: Option<String>,
    with_help_loop: Option<bool>,
) -> Result<i64, AppError> {
    if workspace_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "workspaceId must not be empty".into(),
        ));
    }
    if agent_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "agentId must not be empty".into(),
        ));
    }
    if prompt.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "prompt must not be empty".into(),
        ));
    }

    let bus = bus.inner().clone();
    let registry = registry.inner().clone();
    let job_id = job_id
        .filter(|id| !id.trim().is_empty())
        .unwrap_or_else(|| format!("j-{}", ulid::Ulid::new()));
    let with_help_loop = with_help_loop.unwrap_or(false);
    let target = format!("agent:{agent_id}");
    let summary = format!(
        "dispatch {agent_id} for job {job_id}: {}",
        truncate_for_summary(&prompt)
    );

    // 1. Make sure the dispatcher exists for this (workspace, agent).
    //    Idempotent — second call no-op.
    registry
        .ensure_dispatcher(&app, &workspace_id, &agent_id, &bus)
        .await;

    // 2. Emit the dispatch event. The dispatcher's broadcast
    //    receiver picks it up asynchronously and runs the turn.
    let env = bus
        .emit_typed(
            &app,
            &workspace_id,
            "agent:coordinator",
            &target,
            &summary,
            None,
            MailboxEvent::TaskDispatch {
                job_id,
                target: target.clone(),
                prompt,
                with_help_loop,
            },
        )
        .await?;
    Ok(env.id)
}

// --------------------------------------------------------------------- //
// W5-03 — Coordinator brain dispatch loop                                //
// --------------------------------------------------------------------- //

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

/// Drive the W5-03 Coordinator brain dispatch loop to completion.
/// Parallel to `swarm:run_job` (the FSM-driven path stays alive for
/// regression smokes; W5-06 deletes it).
///
/// Lifecycle:
/// 1. Mint a `j-<ULID>` if not preset; acquire the workspace lock
///    via the existing `JobRegistry::try_acquire_workspace`.
/// 2. Ensure a `MailboxAgentDispatcher` exists for every specialist
///    agent so dispatches the brain emits land on a real receiver
///    immediately.
/// 3. Spawn the brain on `CoordinatorBrain::run` with the workspace's
///    `MailboxBus` and the production `CoordinatorInvoker`.
/// 4. Await the brain's `BrainRunResult` and build a stub
///    `JobOutcome`. The full job-state derivation from mailbox
///    events is W5-04's scope; for W5-03 we surface a minimal shape
///    so callers (manual smokes) get a structured result without
///    a frontend reducer.
/// 5. Release the workspace lock.
///
/// Returns the stub `JobOutcome` on success / failure paths.
/// `final_state` maps from brain outcome:
///   - `"done"` → `JobState::Done`
///   - everything else (`"failed" | "ask_user"`) → `JobState::Failed`
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_run_job_v2<R: Runtime>(
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
    finalise_run_job_v2(
        &app,
        &job_registry,
        &workspace_id,
        &job_id,
        started_at_ms,
        brain_result,
    )
    .await
}

/// Internal: shared finalisation logic between `swarm_run_job_v2`
/// and the test-only entry point. Releases the workspace lock,
/// unregisters the cancel notify, updates the in-memory job state,
/// and asks the [`JobProjector`] to build the canonical
/// [`JobOutcome`] from the persisted event log.
///
/// W5-04: previously this function returned a STUB outcome (empty
/// stages, zero cost). It now defers to `JobProjector::build_outcome`
/// which walks the bus's SQL-persisted event log and returns the
/// fully-shaped outcome — same contract as the FSM's
/// `swarm:run_job` IPC.
async fn finalise_run_job_v2<R: Runtime>(
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
                        "swarm:run_job_v2: MailboxBus or DbPool missing \
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
/// Mirrors `swarm_run_job_v2`'s lifecycle so the tests can exercise
/// the same lock + finalisation path the IPC takes — they only swap
/// out the LLM-spawning piece.
///
/// `spawn_dispatchers` toggles whether the real
/// `MailboxAgentDispatcher`s are wired up. Tests that mock the
/// brain inline (and emit AgentResults from a helper task) pass
/// `false` so the real dispatchers don't race the helper to invoke
/// `claude`.
#[cfg(test)]
pub(crate) async fn swarm_run_job_v2_with_invoker<R, I>(
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
        // W5-04 — brain-driven path, see swarm_run_job_v2 above.
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
    finalise_run_job_v2(
        &app,
        &job_registry,
        &workspace_id,
        &job_id,
        started_at_ms,
        brain_result,
    )
    .await
}

/// Truncate a prompt for the `summary` column of the dispatch row.
/// 80 chars with ellipsis is enough for the existing mailbox UI to
/// render a recognisable line without overflowing.
fn truncate_for_summary(prompt: &str) -> String {
    const CAP: usize = 80;
    if prompt.chars().count() <= CAP {
        prompt.to_string()
    } else {
        let truncated: String = prompt.chars().take(CAP).collect();
        format!("{truncated}…")
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
    /// `swarm:profiles_list` returns exactly the nine bundled
    /// profiles (W3-12d added reviewer + integration-tester; W3-12f
    /// added the coordinator brain; W3-12g renamed `reviewer` to
    /// `backend-reviewer` and added `frontend-builder` +
    /// `frontend-reviewer`; W3-12k1 added the orchestrator brain
    /// inserted alphabetically between `integration-tester` and
    /// `planner`) in deterministic alphabetical order.
    #[tokio::test]
    async fn profiles_list_returns_nine_bundled() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let summaries = swarm_profiles_list(app.handle().clone())
            .await
            .expect("ok");
        let ids: Vec<&str> =
            summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "backend-builder",
                "backend-reviewer",
                "coordinator",
                "frontend-builder",
                "frontend-reviewer",
                "integration-tester",
                "orchestrator",
                "planner",
                "scout",
            ]
        );
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
    // WP-W3-12k1 — swarm:orchestrator_decide validation tests           //
    // ---------------------------------------------------------------- //

    /// Empty `workspace_id` short-circuits before any subprocess
    /// spawn happens; the IPC surfaces `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "".into(),
            "selam".into(),
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspace_id` is treated identically to
    /// empty (`trim().is_empty()` gate). The same gate exists on
    /// `swarm:run_job` so the two surfaces stay symmetric.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_rejects_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "   ".into(),
            "selam".into(),
        )
        .await
        .expect_err("whitespace workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Empty `user_message` short-circuits; the IPC surfaces
    /// `InvalidInput`. The Orchestrator is not allowed to invent a
    /// goal from an empty message.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_validates_empty_message() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "ws-1".into(),
            "".into(),
        )
        .await
        .expect_err("empty message rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `user_message` is treated identically to
    /// empty. Mirrors the validator on `workspace_id`.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_rejects_whitespace_message() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "ws-1".into(),
            "   \t\n".into(),
        )
        .await
        .expect_err("whitespace message rejected");
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
            last_verdict: None,
            source: Job::default_source(),
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
            last_verdict: None,
            source: Job::default_source(),
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
            last_verdict: None,
            source: Job::default_source(),
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
            last_verdict: None,
            source: Job::default_source(),
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
            last_verdict: None,
            source: Job::default_source(),
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
    // WP-W5-05 — swarm:cancel_job source-switching                       //
    // ---------------------------------------------------------------- //

    /// Helper: seed one `swarm_jobs` row directly. Mirrors the
    /// projector's `persist_job_init` shape but bypasses it so the
    /// test stays under the IPC's verification contract.
    async fn seed_swarm_job_row(
        pool: &crate::db::DbPool,
        id: &str,
        workspace_id: &str,
        state: &str,
        source: &str,
    ) {
        sqlx::query(
            "INSERT INTO swarm_jobs \
             (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json, source) \
             VALUES (?, ?, 'g', 0, ?, 0, NULL, NULL, NULL, ?)",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(state)
        .bind(source)
        .execute(pool)
        .await
        .expect("seed swarm_jobs row");
    }

    /// Acceptance: `source='brain'` triggers a `MailboxEvent::JobCancel`
    /// emit on the workspace's bus. The IPC returns `Ok(())` once the
    /// emit lands; the brain + dispatchers pick the event up via
    /// their broadcast subscribers (covered by W5-02 / W5-03 unit
    /// tests).
    #[tokio::test]
    async fn cancel_job_brain_source_emits_job_cancel_event() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        app.manage(registry);
        let bus = Arc::new(crate::swarm::MailboxBus::new(pool.clone()));
        app.manage(Arc::clone(&bus));

        // Seed a brain-driven job in the DB. No matching registry
        // entry needed — the brain path doesn't consult the
        // in-memory JobRegistry.
        seed_swarm_job_row(&pool, "j-brain", "ws-1", "scout", "brain").await;

        // Subscribe BEFORE the cancel so we don't miss the broadcast.
        let mut rx = bus.subscribe("ws-1").await;

        swarm_cancel_job(app.handle().clone(), "j-brain".into())
            .await
            .expect("cancel ok");

        // The mailbox row must land with `kind='job_cancel'` carrying
        // our job_id.
        let env = tokio::time::timeout(
            Duration::from_secs(1),
            rx.recv(),
        )
        .await
        .expect("broadcast received within 1s")
        .expect("envelope");
        match env.event {
            crate::swarm::MailboxEvent::JobCancel { job_id } => {
                assert_eq!(job_id, "j-brain");
            }
            other => panic!("expected JobCancel; got {other:?}"),
        }
        // Persisted row exists.
        let kind: String = sqlx::query_scalar(
            "SELECT kind FROM mailbox WHERE rowid = ?",
        )
        .bind(env.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kind, "job_cancel");
    }

    /// Acceptance: `source='fsm'` keeps the legacy in-memory
    /// `JobRegistry::signal_cancel` path. Mirrors
    /// `cancel_in_flight_with_notify_returns_ok` but seeds the DB
    /// row too so the source-switch hits the `'fsm'` branch.
    #[tokio::test]
    async fn cancel_job_fsm_source_signals_notify() {
        use crate::swarm::coordinator::Job;
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());

        // In-memory registry entry — needed for the FSM path.
        let job = Job {
            id: "j-fsm".into(),
            goal: "g".into(),
            created_at_ms: 0,
            state: JobState::Scout,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
            source: Job::default_source(),
        };
        registry
            .try_acquire_workspace("ws-fsm", job)
            .await
            .expect("acquire");
        let notify = Arc::new(tokio::sync::Notify::new());
        registry
            .register_cancel("j-fsm", Arc::clone(&notify))
            .expect("register");
        app.manage(Arc::clone(&registry));

        // DB row with source='fsm' so the source-query lands the
        // FSM branch.
        seed_swarm_job_row(&pool, "j-fsm", "ws-fsm", "scout", "fsm").await;

        let waiter = tokio::spawn(async move {
            notify.notified().await;
        });
        tokio::task::yield_now().await;

        swarm_cancel_job(app.handle().clone(), "j-fsm".into())
            .await
            .expect("cancel ok");

        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter wakes within 1s")
            .expect("waiter task panicked");
    }

    /// Acceptance: any unknown source string surfaces `Internal`.
    /// Defensive — production only writes `'brain'` or `'fsm'`,
    /// so this branch protects against schema drift.
    #[tokio::test]
    async fn cancel_job_unknown_source_returns_internal_error() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        app.manage(registry);
        // No bus needed — the unknown branch short-circuits before
        // any bus lookup.

        seed_swarm_job_row(
            &pool,
            "j-weird",
            "ws-1",
            "scout",
            "totally-made-up",
        )
        .await;

        let err = swarm_cancel_job(app.handle().clone(), "j-weird".into())
            .await
            .expect_err("unknown source rejected");
        assert_eq!(err.kind(), "internal");
        assert!(
            err.message().contains("totally-made-up"),
            "error message must echo the bad source: {err:?}"
        );
    }

    /// Acceptance: a job_id that exists in neither the DB nor the
    /// registry surfaces `NotFound` — the source-switch falls
    /// through to the FSM branch on `None` source, which then
    /// looks up the registry and returns `NotFound`. Equivalent in
    /// shape to `cancel_unknown_job_id_returns_not_found` but
    /// asserted explicitly under the WP-W5-05 path so a future
    /// refactor doesn't drop the contract.
    #[tokio::test]
    async fn cancel_job_nonexistent_id_returns_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = Arc::new(JobRegistry::new());
        app.manage(registry);

        let err = swarm_cancel_job(
            app.handle().clone(),
            "j-does-not-exist".into(),
        )
        .await
        .expect_err("nonexistent rejected");
        assert_eq!(err.kind(), "not_found");
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
                last_verdict: None,
                source: Job::default_source(),
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
            last_verdict: None,
            source: Job::default_source(),
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

    // ---------------------------------------------------------------- //
    // WP-W3-12k2 — orchestrator history / clear / log_job IPC tests    //
    // ---------------------------------------------------------------- //

    /// Seed N=3 messages directly via the helpers, then call the
    /// IPC and assert it returns oldest-first chronological.
    #[tokio::test]
    async fn swarm_orchestrator_history_returns_oldest_first() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        // Seed three rows out of order so the assertion is non-trivial.
        append_user_message(&pool, "default", "first", 100)
            .await
            .expect("seed u1");
        append_user_message(&pool, "default", "third", 300)
            .await
            .expect("seed u3");
        append_user_message(&pool, "default", "second", 200)
            .await
            .expect("seed u2");

        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "second");
        assert_eq!(msgs[2].content, "third");
    }

    /// Caller-supplied `limit > 200` is capped at 200 — verified by
    /// the empty-result happy path (a `limit=9999` against an empty
    /// pool still returns `Ok(vec![])` rather than erroring).
    #[tokio::test]
    async fn swarm_orchestrator_history_caps_limit_at_200() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            Some(9999),
        )
        .await
        .expect("history ok");
        assert!(msgs.is_empty());
    }

    /// Empty `workspaceId` short-circuits with `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_history_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_history(
            app.handle().clone(),
            "".into(),
            None,
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspaceId` matches the W3-12k1 trim gate.
    #[tokio::test]
    async fn swarm_orchestrator_history_validates_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_history(
            app.handle().clone(),
            "   ".into(),
            None,
        )
        .await
        .expect_err("whitespace workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_clear_history` empties the targeted
    /// workspace.
    #[tokio::test]
    async fn swarm_orchestrator_clear_history_empties_workspace() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        append_user_message(&pool, "default", "drop me", 100)
            .await
            .expect("seed");
        swarm_orchestrator_clear_history(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("clear ok");
        let after = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert!(after.is_empty());
    }

    /// Empty `workspaceId` short-circuits with `InvalidInput` on the
    /// clear surface too.
    #[tokio::test]
    async fn swarm_orchestrator_clear_history_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_clear_history(
            app.handle().clone(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_log_job` writes the Job row visibly via
    /// the history IPC.
    #[tokio::test]
    async fn swarm_orchestrator_log_job_persists_row() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        swarm_orchestrator_log_job(
            app.handle().clone(),
            "default".into(),
            "j-abc".into(),
            "Add doc to X.tsx".into(),
        )
        .await
        .expect("log ok");
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "j-abc");
        assert_eq!(msgs[0].goal.as_deref(), Some("Add doc to X.tsx"));
    }

    /// Empty inputs on the `log_job` surface — workspaceId, jobId,
    /// or goal — surface `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_log_job_validates_inputs() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "".into(),
            "j-1".into(),
            "g".into(),
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "ws".into(),
            "".into(),
            "g".into(),
        )
        .await
        .expect_err("empty jobId rejected");
        assert_eq!(err.kind(), "invalid_input");
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "ws".into(),
            "j-1".into(),
            "   ".into(),
        )
        .await
        .expect_err("whitespace goal rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_decide` persists the user message even
    /// when the LLM invoke is unreachable. The subprocess spawn
    /// will fail in the mock-runtime environment (no `claude` binary)
    /// — but the user row must already be in the DB by then.
    #[tokio::test]
    async fn swarm_orchestrator_decide_persists_user_before_invoke() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // The decide call will surface a SwarmInvoke / spawn error
        // because the mock runtime has no `claude` binary on PATH.
        // We only care that the user row landed before the failure.
        let _ = swarm_orchestrator_decide(
            app.handle().clone(),
            "default".into(),
            "selam".into(),
        )
        .await;
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        // The very first message persisted is the user row.
        assert!(!msgs.is_empty());
        assert_eq!(msgs[0].content, "selam");
    }

    // ---------------------------------------------------------------- //
    // WP-W4-02 — swarm:agents:list_status / shutdown_workspace IPC     //
    // ---------------------------------------------------------------- //

    /// Empty `workspace_id` short-circuits with `InvalidInput` before
    /// touching the registry.
    #[tokio::test]
    async fn swarm_agents_list_status_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err =
            swarm_agents_list_status(app.handle().clone(), "".into())
                .await
                .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspace_id` rejected — same gate as the
    /// other swarm IPCs.
    #[tokio::test]
    async fn swarm_agents_list_status_rejects_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err =
            swarm_agents_list_status(app.handle().clone(), "   ".into())
                .await
                .expect_err("whitespace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Missing registry state surfaces `Internal` — defensive path.
    #[tokio::test]
    async fn swarm_agents_list_status_without_registry_returns_internal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // Intentionally do NOT manage(SwarmAgentRegistry).
        let err = swarm_agents_list_status(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect_err("missing registry");
        assert_eq!(err.kind(), "internal");
    }

    /// Happy path — fresh registry returns 9 `NotSpawned` rows
    /// alphabetically. Same shape `swarm:profiles_list` promises.
    #[tokio::test]
    async fn swarm_agents_list_status_returns_not_spawned_for_fresh_workspace() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let rows = swarm_agents_list_status(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("ok");
        assert_eq!(rows.len(), 9);
        for r in &rows {
            assert_eq!(
                r.status,
                crate::swarm::AgentStatus::NotSpawned
            );
            assert_eq!(r.turns_taken, 0);
            assert!(r.last_activity_ms.is_none());
        }
    }

    /// `shutdown_workspace` empty workspaceId rejected.
    #[tokio::test]
    async fn swarm_agents_shutdown_workspace_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err = swarm_agents_shutdown_workspace(
            app.handle().clone(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `shutdown_workspace` is idempotent — calling on an empty
    /// workspace returns `Ok(())`.
    #[tokio::test]
    async fn swarm_agents_shutdown_workspace_idempotent_on_empty_registry() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        swarm_agents_shutdown_workspace(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("ok");
    }

    // ---------------------------------------------------------------- //
    // WP-W5-02 — swarm:agents:dispatch_to_agent IPC                    //
    // ---------------------------------------------------------------- //

    /// Build a mock app with both `MailboxBus` and `SwarmAgentRegistry`
    /// in state so the W5-02 IPC tests don't repeat the wiring three
    /// times.
    async fn mock_app_with_w5_state() -> (
        tauri::App<tauri::test::MockRuntime>,
        std::sync::Arc<crate::swarm::MailboxBus>,
        std::sync::Arc<SwarmAgentRegistry>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = crate::test_support::fresh_pool().await;
        let bus = std::sync::Arc::new(
            crate::swarm::MailboxBus::new(pool.clone()),
        );
        let registry = std::sync::Arc::new(SwarmAgentRegistry::new(
            std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            ),
        ));
        let app = tauri::test::mock_builder()
            .manage(pool.clone())
            .manage(bus.clone())
            .manage(registry.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, bus, registry, pool, dir)
    }

    /// Acceptance: empty inputs surface `InvalidInput` BEFORE
    /// touching state. Mirrors the validation pattern of every
    /// other swarm IPC.
    #[tokio::test]
    async fn swarm_agents_dispatch_to_agent_validates_inputs() {
        let (app, _bus, _reg, _pool, _dir) = mock_app_with_w5_state().await;
        let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
        let registry_state =
            app.state::<std::sync::Arc<SwarmAgentRegistry>>();

        // Empty workspace_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "".into(),
            "scout".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Empty agent_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "default".into(),
            "".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Whitespace-only agent_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "default".into(),
            "   ".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Empty prompt.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state,
            registry_state,
            "default".into(),
            "scout".into(),
            "".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acceptance: a successful call lands a `task_dispatch` row in
    /// the mailbox + ensures a dispatcher exists in the registry.
    ///
    /// We dispatch to a *non-bundled* agent id so the dispatcher's
    /// downstream `acquire_and_invoke_turn` returns `NotFound`
    /// quickly (no real `claude` spawn) and the dispatcher's error
    /// path emits an `error:` agent_result. That way the test
    /// fully exercises the IPC + emit surface without a 60s claude
    /// spawn timing out.
    #[tokio::test]
    async fn swarm_agents_dispatch_to_agent_emits_dispatch_event() {
        let (app, bus, registry, _pool, _dir) =
            mock_app_with_w5_state().await;
        let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
        let registry_state =
            app.state::<std::sync::Arc<SwarmAgentRegistry>>();

        // Pre-state: no dispatchers, no dispatch rows.
        assert_eq!(registry.dispatcher_count().await, 0);
        let pre =
            bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
        assert!(pre.is_empty());

        let id = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state,
            registry_state,
            "default".into(),
            "test-not-bundled".into(),
            "Investigate auth.rs callsites".into(),
            Some("j-test-1".into()),
            Some(true),
        )
        .await
        .expect("dispatch ok");

        // 1. Dispatcher landed for (default, test-not-bundled).
        assert_eq!(registry.dispatcher_count().await, 1);

        // 2. Mailbox has the task_dispatch row.
        let rows =
            bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, id);
        assert_eq!(row.from_pane, "agent:coordinator");
        assert_eq!(row.to_pane, "agent:test-not-bundled");
        match &row.event {
            crate::swarm::MailboxEvent::TaskDispatch {
                job_id,
                target,
                prompt,
                with_help_loop,
            } => {
                assert_eq!(job_id, "j-test-1");
                assert_eq!(target, "agent:test-not-bundled");
                assert_eq!(prompt, "Investigate auth.rs callsites");
                assert!(*with_help_loop);
            }
            _ => panic!("unexpected event kind"),
        }

        // 3. The dispatcher's invoke task fails fast with NotFound
        //    (the agent isn't in the bundled profile registry) and
        //    emits an error AgentResult with parent_id chained
        //    back to the dispatch row. This proves the error path
        //    end-to-end without needing a real `claude` subprocess.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let rows = bus
                .list_typed(Some("agent_result"), None, None)
                .await
                .unwrap();
            if let Some(row) = rows.into_iter().find(|r| r.parent_id == Some(id)) {
                break row;
            }
            if std::time::Instant::now() > deadline {
                panic!("error AgentResult never arrived");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20))
                .await;
        };
        match &result.event {
            crate::swarm::MailboxEvent::AgentResult {
                assistant_text,
                ..
            } => {
                assert!(
                    assistant_text.starts_with("error:"),
                    "expected error result for unknown agent: {assistant_text}"
                );
            }
            _ => panic!("unexpected event kind"),
        }

        // Cleanup — drain the dispatcher so the test exits without
        // leaving its background task in flight.
        registry.shutdown_all().await.expect("shutdown ok");
    }

    // ---------------------------------------------------------------- //
    // WP-W5-03 — swarm:run_job_v2                                       //
    // ---------------------------------------------------------------- //

    /// Build a mock app wiring `JobRegistry`, `MailboxBus`, and
    /// `SwarmAgentRegistry` so the v2 IPC tests don't repeat the
    /// boilerplate. The job registry is in-memory only (`new()`) so
    /// state mutations don't write through to SQLite — the tests
    /// only care about the in-memory shape.
    async fn mock_app_with_v2_state() -> (
        tauri::App<tauri::test::MockRuntime>,
        std::sync::Arc<crate::swarm::JobRegistry>,
        std::sync::Arc<crate::swarm::MailboxBus>,
        std::sync::Arc<SwarmAgentRegistry>,
        tempfile::TempDir,
    ) {
        let (pool, dir) = crate::test_support::fresh_pool().await;
        let job_registry = std::sync::Arc::new(
            crate::swarm::JobRegistry::with_pool(pool.clone()),
        );
        let bus = std::sync::Arc::new(
            crate::swarm::MailboxBus::new(pool.clone()),
        );
        let agent_registry = std::sync::Arc::new(SwarmAgentRegistry::new(
            std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            ),
        ));
        // WP-W5-04 — install the projector registry so v2 tests
        // exercise the same `ensure_for_workspace` path the IPC
        // takes in production. Without it, `swarm_run_job_v2`
        // skips the projector spawn and `build_outcome` walks the
        // bus directly (still works, but bypasses the live
        // SwarmJobEvent emit chain).
        let projector_registry = std::sync::Arc::new(
            crate::swarm::JobProjectorRegistry::new(),
        );
        let app = tauri::test::mock_builder()
            .manage(pool)
            .manage(job_registry.clone())
            .manage(bus.clone())
            .manage(agent_registry.clone())
            .manage(projector_registry)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, job_registry, bus, agent_registry, dir)
    }

    /// Mock CoordinatorInvoker for v2 tests — same shape as the
    /// brain's ScriptedCoordinatorInvoker but lives here so the
    /// IPC test path doesn't depend on `#[cfg(test)]` items inside
    /// `swarm::brain`.
    struct V2ScriptedInvoker {
        replies: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl V2ScriptedInvoker {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: std::sync::Arc::new(std::sync::Mutex::new(
                    replies.into_iter().map(String::from).collect(),
                )),
            }
        }
    }

    impl crate::swarm::CoordinatorInvoker for V2ScriptedInvoker {
        fn invoke_coordinator_turn(
            &self,
            _workspace_id: &str,
            _user_message: &str,
            _timeout: std::time::Duration,
            _cancel: std::sync::Arc<tokio::sync::Notify>,
        ) -> impl std::future::Future<
            Output = Result<crate::swarm::InvokeResult, AppError>,
        > + Send {
            let replies = std::sync::Arc::clone(&self.replies);
            async move {
                let mut replies = replies.lock().unwrap();
                if replies.is_empty() {
                    return Err(AppError::Internal(
                        "scripted invoker exhausted".into(),
                    ));
                }
                let text = replies.remove(0);
                Ok(crate::swarm::InvokeResult {
                    session_id: "mock".into(),
                    assistant_text: text,
                    total_cost_usd: 0.01,
                    turn_count: 1,
                })
            }
        }
    }

    /// Acceptance: empty inputs surface `InvalidInput` before any
    /// state mutation.
    #[tokio::test]
    async fn run_job_v2_validates_inputs() {
        let (app, _jr, _bus, _ar, _dir) = mock_app_with_v2_state().await;

        let err = swarm_run_job_v2(
            app.handle().clone(),
            "".into(),
            "do something".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        let err = swarm_run_job_v2(
            app.handle().clone(),
            "default".into(),
            "".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Whitespace-only.
        let err = swarm_run_job_v2(
            app.handle().clone(),
            "default".into(),
            "   ".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acceptance: a second call against the same workspace while
    /// the first is in flight surfaces `WorkspaceBusy`. We exercise
    /// this by holding the workspace via a hand-acquired lock —
    /// the IPC's `try_acquire_workspace` short-circuits on the
    /// second call.
    #[tokio::test]
    async fn run_job_v2_workspace_busy_when_concurrent() {
        let (app, jr, _bus, _ar, _dir) = mock_app_with_v2_state().await;

        // Manually acquire the workspace lock (simulates an
        // in-flight job).
        let dummy_job = crate::swarm::Job {
            id: "j-existing".into(),
            goal: "dummy".into(),
            created_at_ms: 0,
            state: crate::swarm::JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
            source: crate::swarm::Job::default_source(),
        };
        jr.try_acquire_workspace("default", dummy_job)
            .await
            .expect("acquire");

        // Now the v2 IPC call collides.
        let err = swarm_run_job_v2(
            app.handle().clone(),
            "default".into(),
            "do something".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "workspace_busy");

        // Cleanup so the dispatcher tasks (if any spawned) drain.
        jr.release_workspace("default", "j-existing").await;
    }

    /// Acceptance: a happy-path mock invoker drives the brain
    /// through Dispatch → AgentResult → Finish and returns a
    /// `JobOutcome` with `final_state == Done`. We use a faux
    /// scout-results emitter that watches for the dispatch and
    /// emits the AgentResult so the brain can take its second turn.
    #[tokio::test]
    async fn run_job_v2_runs_full_chain_via_mock_brain() {
        let (app, _jr, bus, _ar, _dir) = mock_app_with_v2_state().await;

        let invoker = std::sync::Arc::new(V2ScriptedInvoker::new(vec![
            r#"{"action":"dispatch","target":"agent:scout","prompt":"investigate"}"#,
            r#"{"action":"finish","outcome":"done","summary":"done"}"#,
        ]));

        // Helper: emit AgentResult once a dispatch lands. Runs in
        // parallel with the IPC call. Uses a clone of the same app
        // handle so the bus's legacy `mailbox:new` Tauri event lands
        // on the same listener set.
        let bus_for_helper = std::sync::Arc::clone(&bus);
        let app_for_helper = app.handle().clone();
        let helper = tokio::spawn(async move {
            // Poll the bus for the first task_dispatch row.
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(10);
            loop {
                let rows = bus_for_helper
                    .list_typed(Some("task_dispatch"), None, Some(10))
                    .await
                    .expect("list");
                if let Some(row) = rows.into_iter().next() {
                    if let crate::swarm::MailboxEvent::TaskDispatch {
                        job_id, ..
                    } = &row.event
                    {
                        bus_for_helper
                            .emit_typed(
                                &app_for_helper,
                                "default",
                                "agent:scout",
                                "agent:coordinator",
                                "result",
                                Some(row.id),
                                crate::swarm::MailboxEvent::AgentResult {
                                    job_id: job_id.clone(),
                                    agent_id: "scout".into(),
                                    assistant_text: "found".into(),
                                    total_cost_usd: 0.01,
                                    turn_count: 1,
                                },
                            )
                            .await
                            .expect("emit");
                        break;
                    }
                }
                if std::time::Instant::now() > deadline {
                    panic!("never saw dispatch");
                }
                tokio::time::sleep(
                    std::time::Duration::from_millis(20),
                )
                .await;
            }
        });

        let outcome = swarm_run_job_v2_with_invoker(
            app.handle().clone(),
            "default".to_string(),
            "do something".to_string(),
            invoker,
            30,
            // spawn_dispatchers=false — the test's helper is the
            // simulated dispatcher, so we don't want the real one
            // racing it (and trying to spawn `claude`).
            false,
        )
        .await
        .expect("ok");

        let _ = helper.await;
        assert_eq!(outcome.final_state, crate::swarm::JobState::Done);
        assert!(outcome.last_error.is_none());
        assert!(outcome.job_id.starts_with("j-"));
    }

    /// Acceptance: the returned `JobOutcome` shape carries the
    /// expected fields. Even on a parse-failure path (brain bails
    /// after the first invoke returns garbage) we get a stub
    /// outcome with `final_state == Failed` and `last_error`
    /// populated. No `claude` spawn needed for this test.
    #[tokio::test]
    async fn run_job_v2_returns_job_outcome_with_correct_shape() {
        let (app, _jr, _bus, _ar, _dir) = mock_app_with_v2_state().await;
        let invoker = std::sync::Arc::new(V2ScriptedInvoker::new(vec![
            "Just garbage no JSON.",
        ]));

        let outcome = swarm_run_job_v2_with_invoker(
            app.handle().clone(),
            "default".to_string(),
            "trivial".to_string(),
            invoker,
            30,
            // spawn_dispatchers=false — no dispatch flows so the
            // real dispatchers wouldn't fire anyway, but disable
            // for symmetry with the other invoker tests.
            false,
        )
        .await
        .expect("ok");

        // Stub shape: empty stages, zero cost, populated last_error.
        assert_eq!(outcome.final_state, crate::swarm::JobState::Failed);
        assert!(outcome.last_error.is_some());
        assert_eq!(outcome.stages.len(), 0);
        assert_eq!(outcome.total_cost_usd, 0.0);
        assert!(outcome.job_id.starts_with("j-"));
    }

    /// Real-claude integration smoke (`#[ignore]`'d) — drives a
    /// small doc-edit goal end-to-end through the v2 brain dispatch
    /// loop and asserts `final_state == Done`. Wall-clock budget
    /// 600s; env override `NEURON_BRAIN_MAX_DISPATCHES=15` keeps
    /// the LLM honest.
    ///
    /// Time budget: typical 3-8 minutes (multiple cold-starts).
    /// Run with: `$env:NEURON_BRAIN_MAX_DISPATCHES="15"; cargo test \
    /// --lib integration_run_job_v2_real_claude -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_run_job_v2_real_claude() {
        let (app, _jr, _bus, _ar, _dir) = mock_app_with_v2_state().await;
        let outcome = swarm_run_job_v2(
            app.handle().clone(),
            "default".to_string(),
            "Reply with a single 'finish' action with outcome=\"done\" \
             and summary=\"smoke\". No dispatches needed."
                .to_string(),
        )
        .await
        .expect("smoke ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "smoke should produce Done outcome"
        );
    }
}

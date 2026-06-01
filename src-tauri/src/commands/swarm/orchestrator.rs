//! `swarm:orchestrator_*` commands — WP-W3-12k1/k2 single-shot
//! Orchestrator brain + persisted chat thread.
//!
//! - `swarm:orchestrator_decide` — invoke the bundled Orchestrator
//!   persona against the user message (prepended with the last N
//!   thread messages) and persist the parsed outcome.
//! - `swarm:orchestrator_history` — read the persisted chat thread
//!   for a workspace, oldest-first.
//! - `swarm:orchestrator_clear_history` — wipe the chat thread.
//! - `swarm:orchestrator_log_job` — append a "swarm dispatched" Job
//!   marker to the chat thread.

use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::coordinator::orchestrator_session::{
    append_job_message, append_orchestrator_message, append_user_message,
    clear_messages, list_recent_messages, render_with_history,
};
use crate::swarm::coordinator::{
    parse_orchestrator_outcome, OrchestratorMessage, OrchestratorOutcome,
};
use crate::swarm::{ProfileRegistry, SubprocessTransport, Transport};

use super::workspace_agents_dir;

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

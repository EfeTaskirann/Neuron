//! `swarm:agents:dispatch_to_agent` — WP-W5-02 mailbox-bus dispatch
//! surface.

use std::sync::Arc;

use tauri::{AppHandle, Runtime, State};

use crate::error::AppError;
use crate::swarm::{MailboxBus, MailboxEvent, SwarmAgentRegistry};

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

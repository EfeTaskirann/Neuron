//! The `acquire_and_invoke_turn` hot path: lazy session spawn,
//! turn-cap respawn, streaming-event forwarding, and the per-turn
//! status FSM. Split out of [`super`] to keep the ~190-line turn
//! driver isolated from the registry's structural plumbing.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc;
use tokio::sync::{Mutex, Notify};

use crate::error::AppError;
use crate::swarm::persistent_session::{PersistentSession, TurnStreamEvent};
use crate::swarm::transport::InvokeResult;
use crate::time::now_millis;

use super::super::event::{agent_event_channel, SwarmAgentEvent};
use super::super::slot::AgentSlot;
use super::super::status::AgentStatus;
use super::SwarmAgentRegistry;

impl SwarmAgentRegistry {
    /// Acquire (or lazy-spawn) the session for one
    /// (workspace, agent) and run one turn against it.
    ///
    /// Callers are the mailbox dispatcher (`agent_dispatcher::invoker`)
    /// and the coordinator brain (`brain::invoker`). Failure paths
    /// leave the slot in `Crashed` state with `session: None`; the
    /// next call respawns transparently.
    ///
    /// Cancel: forwarded to `PersistentSession::invoke_turn`.
    /// Cancel returns `AppError::Cancelled` and leaves the session
    /// alive (W4-01 cancel contract).
    pub async fn acquire_and_invoke_turn<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> Result<InvokeResult, AppError> {
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
        let key = (workspace_id.to_string(), agent_id.to_string());

        // 1. Get or insert the slot. Read first; if missing, take
        //    write lock to insert. Don't hold the write lock across
        //    the spawn — release it after the slot exists.
        let slot_arc = {
            let read = self.sessions.read().await;
            match read.get(&key) {
                Some(slot) => Arc::clone(slot),
                None => {
                    drop(read);
                    let mut write = self.sessions.write().await;
                    Arc::clone(
                        write
                            .entry(key.clone())
                            .or_insert_with(|| {
                                Arc::new(Mutex::new(AgentSlot::new()))
                            }),
                    )
                }
            }
        };

        // 2. Lock the per-agent slot for the duration of this turn.
        //    Other agents in the same workspace can run in parallel.
        let mut slot = slot_arc.lock().await;

        let channel = agent_event_channel(workspace_id, agent_id);

        // 3. Lazy spawn if needed. Also: turn-cap respawn — if the
        //    existing session has accumulated `turn_cap` turns, kill
        //    it and replace with a fresh one before this turn fires.
        let needs_spawn = slot.session.is_none()
            || slot.turns_taken >= self.turn_cap;
        if needs_spawn {
            // Resolve the profile BEFORE flipping status — a missing
            // profile must error out without leaving the slot parked
            // in `Spawning` (the contract says failures end Crashed).
            let profile = self
                .profiles
                .get(agent_id)
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "swarm profile `{agent_id}`"
                    ))
                })?;
            if let Some(old_session) = slot.session.take() {
                tracing::info!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    turns_taken = slot.turns_taken,
                    turn_cap = self.turn_cap,
                    "respawning agent session after turn cap"
                );
                // Best-effort shutdown of the old session; failures
                // shouldn't block the respawn.
                let _ = old_session.shutdown().await;
            }
            slot.status = AgentStatus::Spawning;
            slot.last_activity_ms = Some(now_millis());

            let session =
                PersistentSession::spawn(app, profile).await.map_err(|e| {
                    slot.status = AgentStatus::Crashed;
                    e
                })?;
            slot.session = Some(session);
            slot.turns_taken = 0;
            // Emit Spawned now that the session is alive in the slot.
            // Drop errors silently — emit failures shouldn't break
            // the registry hot path.
            let _ = app.emit(
                &channel,
                SwarmAgentEvent::Spawned {
                    profile_id: profile.id.clone(),
                },
            );
        }

        // 4. Run the turn. Status flips to Running for the duration,
        //    then to Idle on success / Crashed on hard error.
        slot.status = AgentStatus::Running;
        slot.last_activity_ms = Some(now_millis());
        let _ = app.emit(
            &channel,
            SwarmAgentEvent::TurnStarted {
                turn_index: slot.turns_taken,
            },
        );

        // Set up the streaming-event mpsc. The forwarder task
        // lifts each TurnStreamEvent into a SwarmAgentEvent and
        // emits it on the per-agent channel. The task exits when
        // the sender drops (after invoke_turn returns).
        let (tx, mut rx) = mpsc::unbounded_channel::<TurnStreamEvent>();
        let app_for_forward = app.clone();
        let channel_for_forward = channel.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                let payload = match ev {
                    TurnStreamEvent::AssistantText { delta } => {
                        SwarmAgentEvent::AssistantText { delta }
                    }
                    TurnStreamEvent::ToolUse { name, input_summary } => {
                        SwarmAgentEvent::ToolUse {
                            name,
                            input_summary,
                        }
                    }
                };
                let _ = app_for_forward.emit(&channel_for_forward, payload);
            }
        });

        let session = slot.session.as_mut().ok_or_else(|| {
            // Should never fire — we just spawned above.
            AppError::Internal(
                "swarm agent slot has no session post-spawn".into(),
            )
        })?;
        let outcome = session
            .invoke_turn(user_message, timeout, cancel, Some(tx))
            .await;
        slot.turns_taken = session.turns_taken();
        slot.last_activity_ms = Some(now_millis());

        // Forwarder exits when the sender drops at end of scope; we
        // don't need to await it explicitly, but joining bounds the
        // event ordering so a late delta doesn't fire after Result.
        // Best-effort — a forwarder panic shouldn't reach here, but
        // ignore any join error.
        let _ = forwarder.await;

        match outcome {
            Ok(result) => {
                slot.status = AgentStatus::Idle;
                let _ = app.emit(
                    &channel,
                    SwarmAgentEvent::Result {
                        assistant_text: result.assistant_text.clone(),
                        total_cost_usd: result.total_cost_usd,
                        turn_count: result.turn_count,
                    },
                );
                let _ = app.emit(&channel, SwarmAgentEvent::Idle);
                Ok(result)
            }
            Err(AppError::Cancelled(msg)) => {
                // Cancel keeps the session alive — flip back to
                // Idle so the next acquire reuses it.
                slot.status = AgentStatus::Idle;
                let _ = app.emit(&channel, SwarmAgentEvent::Idle);
                Err(AppError::Cancelled(msg))
            }
            Err(other) => {
                // SwarmInvoke / Timeout / etc. → mark crashed,
                // drop the session so the next acquire respawns.
                slot.status = AgentStatus::Crashed;
                let error_msg = other.message().to_string();
                if let Some(dead) = slot.session.take() {
                    // Best-effort shutdown so the child doesn't
                    // linger as an orphan if its stdin/out pipes
                    // are still drainable.
                    let _ = dead.shutdown().await;
                }
                let _ = app.emit(
                    &channel,
                    SwarmAgentEvent::Crashed { error: error_msg },
                );
                Err(other)
            }
        }
    }
}

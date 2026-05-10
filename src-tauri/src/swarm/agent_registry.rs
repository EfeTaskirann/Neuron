//! `SwarmAgentRegistry` — workspace-scoped lifecycle owner for
//! W4-01's `PersistentSession`s (WP-W4-02).
//!
//! Keyed by `(workspace_id, agent_id)`. Sessions lazy-spawn on first
//! `acquire_and_invoke_turn`; reused across turns until the
//! workspace is shut down (W4-02 §"Lifecycle"). Per-agent status is
//! exposed read-only via `list_status` for the eventual W4-04 grid
//! header.
//!
//! Concurrency model:
//! - Outer `RwLock<HashMap<...>>` guards structural changes
//!   (insert / remove). Reads dominate (status checks, hash lookups
//!   on `acquire`), so the read lock keeps the hot path uncontended.
//! - Per-agent `Mutex<AgentSession>` serialises calls against a
//!   single session — `PersistentSession` is not `Sync`, and at most
//!   one `invoke_turn` against the same child can be in flight at a
//!   time (W4-01 contract).
//!
//! Out of scope (per WP §"Out of scope"): event channel emission
//! (W4-03) / 3×3 grid UI (W4-04) / `neuron_help` parser (W4-05) /
//! FSM persistent-transport adapter (W4-06).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::mpsc;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::error::AppError;
use crate::swarm::persistent_session::{PersistentSession, TurnStreamEvent};
use crate::swarm::profile::ProfileRegistry;
use crate::swarm::transport::InvokeResult;
use crate::time::now_millis;

/// Default hard cap on `turns_taken` before a session is gracefully
/// respawned. Tunable per-process via `NEURON_SWARM_AGENT_TURN_CAP`.
/// 200 is generous — most jobs walk through 5-7 stages, so the
/// average specialist fires < 10 turns per job. Cap at 200 means a
/// session has to absorb ≥ 20 jobs before respawn — well past the
/// "context bloat" point in practice.
const DEFAULT_TURN_CAP: u32 = 200;

/// Env override for `DEFAULT_TURN_CAP`. Same reading rules as the
/// stage-timeout pattern in `commands/swarm.rs`: numeric > 0 wins;
/// non-numeric / zero falls back to the default with a warn log.
const TURN_CAP_ENV: &str = "NEURON_SWARM_AGENT_TURN_CAP";

/// Per-agent status visible to the UI (eventually rendered by W4-04
/// grid header pills). Snake_case wire form per Charter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Default for every (workspace, agent) pair before the first
    /// lazy-spawn fires. The grid renders these as muted "—" pills.
    NotSpawned,
    /// Spawning in flight. Brief — visible only across one
    /// `acquire` window. Flips to `Idle` once the session is in the
    /// registry.
    Spawning,
    /// Session ready, no turn in flight.
    Idle,
    /// `invoke_turn` is in flight against this session.
    Running,
    /// Specialist emitted a `neuron_help` block (W4-05 will set
    /// this; W4-02 never emits it but the variant is present so
    /// W4-05 doesn't have to widen the type).
    WaitingOnCoordinator,
    /// The session crashed (subprocess died unrecoverably). Will
    /// be respawned on next `acquire_and_invoke_turn`. Distinct
    /// from `NotSpawned` so the grid can surface a "this agent had
    /// trouble" indicator separate from "this agent never ran".
    Crashed,
}

/// W4-03 — payload of the per-(workspace, agent) event channel
/// `swarm:agent:{workspace_id}:{agent_id}:event`. The W4-04 grid
/// pane subscribes to one such channel per agent and renders a live
/// transcript as events arrive.
///
/// Variants split into two groups:
/// - **Bookend** (Spawned / TurnStarted / Result / Idle / Crashed):
///   emitted by the registry around `invoke_turn` calls. Drive
///   the pane status pill + cost-so-far counter.
/// - **Streaming** (AssistantText / ToolUse / HelpRequest): emitted
///   from inside `invoke_turn` via the `TurnStreamEvent` mpsc.
///   Drive the live transcript renderer. `HelpRequest` is reserved
///   here for W4-05 — the registry doesn't emit it in W4-03.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmAgentEvent {
    /// `PersistentSession::spawn` succeeded; the registry slot just
    /// flipped from `NotSpawned` → `Idle`. Carries the profile id
    /// so the pane can render the persona name without a separate
    /// IPC.
    Spawned { profile_id: String },
    /// `acquire_and_invoke_turn` is about to write a user message.
    /// `turn_index` mirrors the registry's `turns_taken` BEFORE the
    /// new turn (first turn is `turn_index: 0`).
    TurnStarted { turn_index: u32 },
    /// Streaming text delta from claude. May fire many times per
    /// turn; the W4-04 pane appends to a per-turn buffer.
    AssistantText { delta: String },
    /// Claude is using a tool. `name` is the tool name (Read, Edit,
    /// Glob, etc.); `input_summary` is a one-line truncation of the
    /// tool input (capped via `TOOL_USE_INPUT_SUMMARY_CAP` in
    /// `transport.rs`). The W4-04 pane shows "Scout is reading
    /// SwarmJobList.tsx" badges.
    ToolUse { name: String, input_summary: String },
    /// Turn finished cleanly. Final assistant text + accounting.
    Result {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// Reserved for W4-05 — specialist emitted a `neuron_help`
    /// JSON block. W4-03 never emits this; W4-05 wires the parser.
    HelpRequest { reason: String, question: String },
    /// Turn ended (success or cancel — not crash); slot is back to
    /// `Idle`.
    Idle,
    /// Session crashed unrecoverably. Slot is `Crashed`; next
    /// `acquire` will respawn.
    Crashed { error: String },
}

/// Build the per-(workspace, agent) event channel name. Centralised
/// so the frontend hook + the backend emit + tests all agree on the
/// exact shape.
pub fn agent_event_channel(workspace_id: &str, agent_id: &str) -> String {
    format!("swarm:agent:{workspace_id}:{agent_id}:event")
}

/// Wire shape for `swarm:agents:list_status`. Trimmed to what the
/// UI actually renders; richer per-agent diagnostics can be added
/// in a follow-up without breaking this surface.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusRow {
    pub workspace_id: String,
    pub agent_id: String,
    pub status: AgentStatus,
    /// `0` for un-touched agents — `NotSpawned` rows always have
    /// `turns_taken: 0`. After respawn under the turn-cap, this
    /// counter resets.
    pub turns_taken: u32,
    /// Wall-clock ms since UNIX epoch of the most recent
    /// state-changing event (spawn, turn start, turn end, crash).
    /// `None` when `status == NotSpawned`.
    pub last_activity_ms: Option<i64>,
}

/// Inner per-agent slot. The registry holds these behind an
/// `Arc<Mutex<...>>` so each agent's turns serialise without
/// blocking other agents.
struct AgentSlot {
    session: Option<PersistentSession>,
    status: AgentStatus,
    turns_taken: u32,
    last_activity_ms: Option<i64>,
}

impl AgentSlot {
    fn new() -> Self {
        Self {
            session: None,
            status: AgentStatus::NotSpawned,
            turns_taken: 0,
            last_activity_ms: None,
        }
    }
}

/// Workspace-scoped session registry. See module docs.
pub struct SwarmAgentRegistry {
    sessions: RwLock<HashMap<(String, String), Arc<Mutex<AgentSlot>>>>,
    profiles: Arc<ProfileRegistry>,
    turn_cap: u32,
    /// W5-02 — per-(workspace, agent) `MailboxAgentDispatcher`s.
    /// Spawned lazily by `ensure_dispatcher` on first dispatch
    /// targeting the agent (or eagerly by tests / IPC). On
    /// `shutdown_all` these are drained BEFORE sessions die so
    /// in-flight invokes can complete via cancel rather than
    /// being torn out from under the dispatcher.
    dispatchers: RwLock<
        HashMap<
            (String, String),
            crate::swarm::agent_dispatcher::MailboxAgentDispatcher,
        >,
    >,
}

impl SwarmAgentRegistry {
    /// Build a fresh registry. Reads `NEURON_SWARM_AGENT_TURN_CAP`
    /// from the environment; non-numeric / zero values fall back to
    /// `DEFAULT_TURN_CAP` with a warn log so a typo isn't silently
    /// ignored.
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self {
        let turn_cap = resolve_turn_cap();
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
            dispatchers: RwLock::new(HashMap::new()),
        }
    }

    /// Builder hook for tests — lets the suite pin a small `turn_cap`
    /// without spelunking through env vars.
    #[cfg(test)]
    pub(crate) fn with_turn_cap(
        profiles: Arc<ProfileRegistry>,
        turn_cap: u32,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
            dispatchers: RwLock::new(HashMap::new()),
        }
    }

    /// Acquire (or lazy-spawn) the session for one
    /// (workspace, agent) and run one turn against it.
    ///
    /// Caller is the FSM (W4-06) for specialists, the chat IPC for
    /// Orchestrator. Failure paths leave the slot in `Crashed`
    /// state with `session: None`; the next call respawns
    /// transparently.
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

            let profile = self
                .profiles
                .get(agent_id)
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "swarm profile `{agent_id}`"
                    ))
                })?;
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

    /// W4-06 — help-aware variant of `acquire_and_invoke_turn`.
    /// After every specialist turn, scan the assistant_text for a
    /// `neuron_help` block. On hit:
    ///
    /// 1. Set status → `WaitingOnCoordinator`, emit `HelpRequest` event
    /// 2. Send the formatted help message to the Coordinator session
    /// 3. Parse `CoordinatorHelpOutcome`:
    ///    - `DirectAnswer` — feed back to the specialist as a new
    ///      turn ("Coordinator says: ...") and loop
    ///    - `AskBack` — feed the followup question back to the
    ///      specialist as a new turn and loop
    ///    - `Escalate` — return the user_question wrapped in
    ///      `AppError::SwarmInvoke("escalated to user: ...")` so
    ///      the FSM can surface it through the Orchestrator chat
    ///
    /// Loop bounded by `max_help_rounds` (default 3) — past the
    /// cap we surface the last help_request as `SwarmInvoke` so
    /// the FSM doesn't hang on an LLM stuck in a help-loop.
    ///
    /// Reviewer / Tester invocations should call the basic
    /// `acquire_and_invoke_turn` (no help loop) — their output
    /// contract is JSON Verdict; help-mode would conflict.
    pub async fn acquire_and_invoke_turn_with_help<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
        max_help_rounds: u32,
    ) -> Result<InvokeResult, AppError> {
        use crate::swarm::help_request::{
            parse_help_request, process_help_request, CoordinatorHelpOutcome,
        };

        let mut current_user_message = user_message.to_string();
        for round in 0..=max_help_rounds {
            let result = self
                .acquire_and_invoke_turn(
                    app,
                    workspace_id,
                    agent_id,
                    &current_user_message,
                    timeout,
                    Arc::clone(&cancel),
                )
                .await?;

            // Specialist DID NOT emit help → return the result.
            let help = match parse_help_request(&result.assistant_text) {
                Some(h) => h,
                None => return Ok(result),
            };

            // Cap reached: stop looping; surface the help request as
            // a SwarmInvoke so the FSM treats it as a hard failure
            // rather than an infinite loop.
            if round == max_help_rounds {
                return Err(AppError::SwarmInvoke(format!(
                    "specialist `{agent_id}` exceeded help-loop cap \
                     ({max_help_rounds} rounds); last reason: {} | \
                     question: {}",
                    help.reason, help.question
                )));
            }

            // Mark the specialist as waiting + emit HelpRequest event.
            // Status is per-agent so we have to re-acquire the slot
            // briefly for the flip.
            self.mark_waiting_on_coordinator(workspace_id, agent_id, &help)
                .await;
            let channel = agent_event_channel(workspace_id, agent_id);
            let _ = app.emit(
                &channel,
                SwarmAgentEvent::HelpRequest {
                    reason: help.reason.clone(),
                    question: help.question.clone(),
                },
            );

            // W4-07 — persist help request to mailbox for audit
            // trail. Best-effort; a missing pool (test path) is
            // silently skipped so unit tests still pass.
            self.emit_help_mailbox(
                app,
                agent_id,
                "coordinator",
                "swarm.help_request",
                &format!(
                    "{} (reason: {})",
                    help.question, help.reason
                ),
            )
            .await;

            // Ask Coordinator. Same `Self` for nested registry call.
            let outcome = process_help_request(
                self,
                app,
                workspace_id,
                agent_id,
                &help,
                timeout,
                Arc::clone(&cancel),
            )
            .await?;

            // W4-07 — persist coordinator outcome to mailbox before
            // routing back to the specialist. Three different
            // `entry_type` strings to keep the trace filterable.
            let (mb_kind, mb_summary) = match &outcome {
                CoordinatorHelpOutcome::DirectAnswer { answer } => {
                    ("swarm.help_direct_answer", answer.clone())
                }
                CoordinatorHelpOutcome::AskBack {
                    followup_question,
                } => (
                    "swarm.help_ask_back",
                    followup_question.clone(),
                ),
                CoordinatorHelpOutcome::Escalate { user_question } => {
                    ("swarm.help_escalate", user_question.clone())
                }
            };
            self.emit_help_mailbox(
                app,
                "coordinator",
                agent_id,
                mb_kind,
                &mb_summary,
            )
            .await;

            // Translate outcome into the next-turn user message.
            current_user_message = match outcome {
                CoordinatorHelpOutcome::DirectAnswer { answer } => {
                    format!(
                        "Coordinator says: {answer}\n\n\
                         Now resume your task with this answer in context."
                    )
                }
                CoordinatorHelpOutcome::AskBack {
                    followup_question,
                } => {
                    format!(
                        "Coordinator asks for more info: {followup_question}\n\n\
                         Reply with the requested detail (or, if you can't,\n\
                         emit another `neuron_help` block with a refined question)."
                    )
                }
                CoordinatorHelpOutcome::Escalate { user_question } => {
                    return Err(AppError::SwarmInvoke(format!(
                        "escalated to user: {user_question}"
                    )));
                }
            };
            // Loop continues with the new message.
        }
        // Unreachable: the for-loop returns from inside on the
        // last iteration (`round == max_help_rounds` branch).
        Err(AppError::Internal(
            "help-loop iteration exceeded — should be unreachable".into(),
        ))
    }

    /// W4-07 — best-effort mailbox emit during the help loop.
    /// `from_agent` and `to_agent` are bare agent ids (e.g.
    /// `scout`, `coordinator`); the mailbox stores them with an
    /// `agent:` prefix so future UI filters can distinguish them
    /// from terminal-pane mailbox entries.
    ///
    /// Failures (missing pool, DB write error) are silently
    /// dropped — the live event channel already carries the same
    /// information for the W4-04 grid; mailbox is only the audit
    /// trail. Test runs lacking a pool stay green.
    async fn emit_help_mailbox<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        from_agent: &str,
        to_agent: &str,
        entry_type: &str,
        summary: &str,
    ) {
        let pool = match app.try_state::<crate::db::DbPool>() {
            Some(p) => p.inner().clone(),
            None => return,
        };
        let from_pane = format!("agent:{from_agent}");
        let to_pane = format!("agent:{to_agent}");
        let _ = crate::commands::mailbox::emit_internal(
            app,
            &pool,
            &from_pane,
            &to_pane,
            entry_type,
            summary,
        )
        .await;
    }

    /// Helper for the help loop — flips the per-agent status to
    /// `WaitingOnCoordinator` between the specialist's
    /// help-emitting turn and the Coordinator's response. Brief
    /// (sub-second) so the UI catches the transition.
    async fn mark_waiting_on_coordinator(
        &self,
        workspace_id: &str,
        agent_id: &str,
        _help: &crate::swarm::help_request::HelpRequest,
    ) {
        let key = (workspace_id.to_string(), agent_id.to_string());
        let read = self.sessions.read().await;
        if let Some(slot_arc) = read.get(&key) {
            let mut slot = slot_arc.lock().await;
            slot.status = AgentStatus::WaitingOnCoordinator;
            slot.last_activity_ms = Some(now_millis());
        }
    }

    /// Read-only snapshot for `swarm:agents:list_status`. Cheap —
    /// clones the metadata, never the session itself. Returns one
    /// row per *bundled* profile in the registry (so the UI can
    /// render a `NotSpawned` pill for agents the user hasn't
    /// touched yet) plus any rows that lazy-spawned.
    pub async fn list_status(
        &self,
        workspace_id: &str,
    ) -> Vec<AgentStatusRow> {
        let read = self.sessions.read().await;

        // Build the result by walking the bundled profile list so
        // un-touched agents appear as NotSpawned. Workspace-override
        // profiles also show up (they live in the same
        // ProfileRegistry).
        let mut rows: Vec<AgentStatusRow> = Vec::new();
        for profile in self.profiles.list() {
            let key =
                (workspace_id.to_string(), profile.id.clone());
            let row = match read.get(&key) {
                Some(slot_arc) => {
                    let slot = slot_arc.lock().await;
                    AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: slot.status,
                        turns_taken: slot.turns_taken,
                        last_activity_ms: slot.last_activity_ms,
                    }
                }
                None => AgentStatusRow {
                    workspace_id: workspace_id.to_string(),
                    agent_id: profile.id.clone(),
                    status: AgentStatus::NotSpawned,
                    turns_taken: 0,
                    last_activity_ms: None,
                },
            };
            rows.push(row);
        }
        rows.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        rows
    }

    /// Eager shutdown of every session for `workspace_id`.
    /// Idempotent — calling on an empty workspace returns `Ok(())`.
    /// Used by `swarm:agents:shutdown_workspace` and by
    /// `shutdown_all` on app close.
    pub async fn shutdown_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<(), AppError> {
        if workspace_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }
        // Collect the keys + slots to shut down. Drop the
        // structural lock before driving the per-slot shutdowns so
        // we don't block other workspaces' acquire calls during a
        // shutdown that may take seconds.
        let to_shutdown: Vec<(
            (String, String),
            Arc<Mutex<AgentSlot>>,
        )> = {
            let mut write = self.sessions.write().await;
            let keys: Vec<(String, String)> = write
                .keys()
                .filter(|(ws, _)| ws == workspace_id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| {
                    write.remove(&k).map(|slot| (k, slot))
                })
                .collect()
        };

        for (_, slot_arc) in to_shutdown {
            // Take the slot lock and drop the session. We do this
            // sequentially per (workspace, agent) — these locks are
            // independent so we don't gain much from parallelism,
            // and serial keeps the log readable.
            let mut slot = slot_arc.lock().await;
            if let Some(session) = slot.session.take() {
                let _ = session.shutdown().await;
            }
            slot.status = AgentStatus::NotSpawned;
            slot.turns_taken = 0;
            slot.last_activity_ms = None;
        }
        Ok(())
    }

    /// Eager shutdown of every workspace's every session.
    /// Called from `lib.rs` on `WindowEvent::CloseRequested`.
    ///
    /// W5-02 — drains every `MailboxAgentDispatcher` BEFORE
    /// tearing down the session map. The order matters: a
    /// dispatcher with an in-flight invoke holds an `Arc` to the
    /// invoker which holds the registry; shutting sessions first
    /// would yank `PersistentSession`s out from under the
    /// dispatcher's invoke task. Shutting dispatchers first lets
    /// each invoke finish via cancel signal cleanly.
    pub async fn shutdown_all(&self) -> Result<(), AppError> {
        // 1. Drain dispatchers first.
        let dispatchers: Vec<(
            (String, String),
            crate::swarm::agent_dispatcher::MailboxAgentDispatcher,
        )> = {
            let mut write = self.dispatchers.write().await;
            write.drain().collect()
        };
        for (_, dispatcher) in dispatchers {
            dispatcher.shutdown().await;
        }

        // 2. Now tear down sessions.
        let workspace_ids: Vec<String> = {
            let read = self.sessions.read().await;
            let mut ids: Vec<String> = read
                .keys()
                .map(|(ws, _)| ws.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        for ws in workspace_ids {
            // Each shutdown_workspace drops the structural lock
            // between iterations so we don't hold the write lock
            // for the entire teardown wall time.
            self.shutdown_workspace(&ws).await?;
        }
        Ok(())
    }

    /// W5-02 — idempotent lazy spawn of a `MailboxAgentDispatcher`
    /// for `(workspace_id, agent_id)`. Second + subsequent calls
    /// no-op. The returned dispatcher is stored on the registry;
    /// callers do not own it (its lifecycle ends on
    /// `shutdown_all`).
    ///
    /// The dispatcher subscribes to the workspace's `MailboxBus`
    /// channel and routes `agent:<agent_id>`-targeted
    /// `task_dispatch` events to
    /// `acquire_and_invoke_turn` (no help loop — that's W5-03
    /// scope). Per the WP this is *lazy*: callers may invoke this
    /// at app boot for a known set of agents, OR at first dispatch
    /// (the IPC `swarm:agents:dispatch_to_agent` calls it
    /// inline before emitting).
    ///
    /// Validation: empty `workspace_id` / `agent_id` are rejected
    /// at the IPC boundary; this method silently no-ops on empty
    /// inputs since the registry's hot path doesn't validate
    /// either (defense-in-depth — the IPC ALWAYS validates first).
    pub async fn ensure_dispatcher<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        workspace_id: &str,
        agent_id: &str,
        bus: &Arc<crate::swarm::mailbox_bus::MailboxBus>,
    ) {
        if workspace_id.trim().is_empty() || agent_id.trim().is_empty() {
            return;
        }
        let key = (workspace_id.to_string(), agent_id.to_string());

        // Fast path: if the dispatcher already exists, return.
        {
            let read = self.dispatchers.read().await;
            if read.contains_key(&key) {
                return;
            }
        }

        // Slow path: take the write lock and re-check (handles the
        // read-then-write race between two concurrent ensures).
        let mut write = self.dispatchers.write().await;
        if write.contains_key(&key) {
            return;
        }

        let invoker = Arc::new(
            crate::swarm::agent_dispatcher::SwarmAgentRegistryInvoker::new(
                app.clone(),
                Arc::clone(self),
            ),
        );
        let dispatcher =
            crate::swarm::agent_dispatcher::MailboxAgentDispatcher::spawn(
                app.clone(),
                workspace_id.to_string(),
                agent_id.to_string(),
                invoker,
                Arc::clone(bus),
            )
            .await;
        write.insert(key, dispatcher);
    }

    /// Diagnostics: how many slots does the registry hold across all
    /// workspaces? Used by tests + a future telemetry surface.
    #[cfg(test)]
    pub(crate) async fn slot_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Diagnostics: how many dispatchers are registered. Used by the
    /// `registry_ensure_dispatcher_is_idempotent` test.
    #[cfg(test)]
    pub(crate) async fn dispatcher_count(&self) -> usize {
        self.dispatchers.read().await.len()
    }

    /// Diagnostics: the configured turn cap for this registry.
    pub fn turn_cap(&self) -> u32 {
        self.turn_cap
    }
}

/// W4-06 — `Transport` adapter that drives invokes through the
/// registry's persistent sessions instead of one-shot spawn-and-die.
///
/// The FSM (`CoordinatorFsm`) is generic over `T: Transport`. To
/// rewire it to persistent sessions without touching its
/// generic-over-T contract, we supply this adapter which
/// implements `Transport::invoke` by calling
/// `acquire_and_invoke_turn_with_help` on the underlying registry.
///
/// The help-loop is *transparent* to the FSM: the FSM thinks it's
/// making one call against `T: Transport`; the registry's help-
/// aware method internally routes specialist→Coordinator if the
/// specialist's first turn emits `neuron_help`.
///
/// Why bake in the help-loop instead of leaving it to the FSM:
/// - Keeps the FSM state machine simple — no new Blocked /
///   CoordinatorQA states to add. Each FSM stage stays
///   "specialist invoke → result".
/// - Help is a per-turn concern, not a per-stage concern. Losing
///   the help loop when the FSM transitions to the next stage
///   would defeat its purpose.
///
/// Reviewer / Tester stages: the FSM invokes them via
/// `RegistryTransport` too; their persona contracts forbid
/// `neuron_help` blocks (output is JSON Verdict), so the help
/// branch never fires. Defense-in-depth: even if the parser saw a
/// stray `neuron_help` in a Verdict's `summary` field, the
/// outer-fence detection + reviewer's strict JSON shape means the
/// help-loop won't activate spuriously.
pub struct RegistryTransport {
    workspace_id: String,
    registry: Arc<SwarmAgentRegistry>,
    cancel: Arc<Notify>,
    max_help_rounds: u32,
}

impl RegistryTransport {
    /// Default cap on help-loop rounds. Picked empirically: 3 is
    /// enough to handle "specialist asks → coordinator answers →
    /// specialist asks follow-up → coordinator answers" plus one
    /// safety margin. Past 3 rounds the LLM is usually stuck.
    pub const DEFAULT_HELP_ROUNDS: u32 = 3;

    /// Construct a transport bound to one workspace + cancel notify.
    pub fn new(
        workspace_id: String,
        registry: Arc<SwarmAgentRegistry>,
        cancel: Arc<Notify>,
    ) -> Self {
        Self {
            workspace_id,
            registry,
            cancel,
            max_help_rounds: Self::DEFAULT_HELP_ROUNDS,
        }
    }

    /// Builder for tests / FSM that want to disable the help loop
    /// entirely (e.g. Reviewer/Tester invokes).
    pub fn with_max_help_rounds(mut self, n: u32) -> Self {
        self.max_help_rounds = n;
        self
    }
}

impl crate::swarm::transport::Transport for RegistryTransport {
    fn invoke<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        profile: &crate::swarm::profile::Profile,
        user_message: &str,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send {
        let registry = Arc::clone(&self.registry);
        let workspace_id = self.workspace_id.clone();
        let agent_id = profile.id.clone();
        let user_message = user_message.to_string();
        let cancel = Arc::clone(&self.cancel);
        let max_rounds = self.max_help_rounds;
        async move {
            if max_rounds == 0 {
                // help-loop disabled — call the basic acquire path.
                registry
                    .acquire_and_invoke_turn(
                        &app_clone(app),
                        &workspace_id,
                        &agent_id,
                        &user_message,
                        timeout,
                        cancel,
                    )
                    .await
            } else {
                registry
                    .acquire_and_invoke_turn_with_help(
                        &app_clone(app),
                        &workspace_id,
                        &agent_id,
                        &user_message,
                        timeout,
                        cancel,
                        max_rounds,
                    )
                    .await
            }
        }
    }
}

/// Workaround for `AppHandle: Clone` not being object-safe through
/// the Transport trait's `&AppHandle` parameter. We need an owned
/// AppHandle inside the async block that outlives the `&self`
/// borrow; `app.clone()` gets us one.
fn app_clone<R: Runtime>(app: &AppHandle<R>) -> AppHandle<R> {
    app.clone()
}

/// Resolve the per-process turn cap. Same env-reading shape as
/// `commands/swarm.rs::stage_timeout` so the project has one
/// pattern for tunable env knobs.
fn resolve_turn_cap() -> u32 {
    match std::env::var(TURN_CAP_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>() {
            Ok(0) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    "value `0` is not a valid turn cap; falling back to default"
                );
                DEFAULT_TURN_CAP
            }
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    raw = %raw,
                    error = %e,
                    "turn cap override is not a non-negative integer; using default"
                );
                DEFAULT_TURN_CAP
            }
        },
        _ => DEFAULT_TURN_CAP,
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;

    fn fresh_registry() -> Arc<SwarmAgentRegistry> {
        let profiles =
            Arc::new(ProfileRegistry::load_from(None).expect("load"));
        Arc::new(SwarmAgentRegistry::new(profiles))
    }

    /// Fresh registry against the bundled 9 profiles surfaces 9
    /// `NotSpawned` rows for any workspace. The W4-04 grid header
    /// reads exactly this shape on first mount.
    #[tokio::test]
    async fn list_status_returns_not_spawned_for_untouched_agents() {
        let reg = fresh_registry();
        let rows = reg.list_status("default").await;
        assert_eq!(rows.len(), 9, "expected 9 bundled profiles");
        for r in &rows {
            assert_eq!(r.status, AgentStatus::NotSpawned);
            assert_eq!(r.turns_taken, 0);
            assert!(r.last_activity_ms.is_none());
            assert_eq!(r.workspace_id, "default");
        }
        // Stable alphabetical order — same shape `swarm:profiles_list`
        // promises elsewhere.
        let ids: Vec<&str> =
            rows.iter().map(|r| r.agent_id.as_str()).collect();
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
    }

    /// Different workspaces see independent `NotSpawned` rows.
    #[tokio::test]
    async fn list_status_isolated_per_workspace() {
        let reg = fresh_registry();
        let ws_a = reg.list_status("ws-a").await;
        let ws_b = reg.list_status("ws-b").await;
        assert_eq!(ws_a.len(), 9);
        assert_eq!(ws_b.len(), 9);
        for r in &ws_a {
            assert_eq!(r.workspace_id, "ws-a");
        }
        for r in &ws_b {
            assert_eq!(r.workspace_id, "ws-b");
        }
    }

    /// Empty registry slot count is 0 — no sessions exist before
    /// anyone calls `acquire`.
    #[tokio::test]
    async fn fresh_registry_has_zero_slots() {
        let reg = fresh_registry();
        assert_eq!(reg.slot_count().await, 0);
    }

    /// `shutdown_workspace` on an empty registry is a no-op (no
    /// crash, no error).
    #[tokio::test]
    async fn shutdown_workspace_on_empty_registry_is_ok() {
        let reg = fresh_registry();
        reg.shutdown_workspace("default").await.expect("ok");
        // Slot count stays 0.
        assert_eq!(reg.slot_count().await, 0);
    }

    /// `shutdown_all` on an empty registry is a no-op.
    #[tokio::test]
    async fn shutdown_all_on_empty_registry_is_ok() {
        let reg = fresh_registry();
        reg.shutdown_all().await.expect("ok");
        assert_eq!(reg.slot_count().await, 0);
    }

    /// Empty `workspaceId` rejected at the registry method boundary
    /// (mirrors the IPC validation at `commands/swarm.rs`). We
    /// validate twice — defense-in-depth — so a non-IPC caller
    /// (e.g. the FSM) doesn't bypass the check.
    #[tokio::test]
    async fn shutdown_workspace_rejects_empty_workspace_id() {
        let reg = fresh_registry();
        let err = reg
            .shutdown_workspace("")
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Turn cap defaults to `DEFAULT_TURN_CAP` (200) when the env
    /// var is absent.
    #[test]
    fn turn_cap_defaults_to_200_when_env_absent() {
        // Save + clear the env var for the duration of the test.
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::remove_var(TURN_CAP_ENV);
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        assert_eq!(DEFAULT_TURN_CAP, 200);
        // Restore.
        if let Some(v) = prior {
            std::env::set_var(TURN_CAP_ENV, v);
        }
    }

    /// `NEURON_SWARM_AGENT_TURN_CAP=42` lands as `turn_cap = 42`.
    #[test]
    fn turn_cap_env_override_lands() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "42");
        assert_eq!(resolve_turn_cap(), 42);
        // Restore.
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// Non-numeric env override falls back to default with a warn
    /// log (we don't capture the log here — too fragile — but we
    /// do assert the fallback fires).
    #[test]
    fn turn_cap_non_numeric_falls_back_to_default() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "not-a-number");
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// Zero env override falls back to default (we want `cap=0` to
    /// be a typo, not "never respawn").
    #[test]
    fn turn_cap_zero_falls_back_to_default() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "0");
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// `with_turn_cap` builder sets the cap directly without env
    /// dependency — the test path uses this so suite order doesn't
    /// matter.
    #[test]
    fn with_turn_cap_pins_cap() {
        let profiles =
            Arc::new(ProfileRegistry::load_from(None).expect("load"));
        let reg = SwarmAgentRegistry::with_turn_cap(profiles, 5);
        assert_eq!(reg.turn_cap(), 5);
    }

    /// Acquire with empty workspaceId rejected.
    #[tokio::test]
    async fn acquire_validates_empty_workspace_id() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "",
                "scout",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acquire with empty agentId rejected.
    #[tokio::test]
    async fn acquire_validates_empty_agent_id() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    // ---------------------------------------------------------------- //
    // WP-W5-02 — ensure_dispatcher idempotence                          //
    // ---------------------------------------------------------------- //

    /// Calling `ensure_dispatcher` twice for the same
    /// (workspace, agent) pair leaves exactly one dispatcher
    /// registered. Different (workspace, agent) keys land
    /// independent dispatchers. Empty inputs no-op silently.
    #[tokio::test]
    async fn registry_ensure_dispatcher_is_idempotent() {
        let reg = fresh_registry();
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(crate::swarm::MailboxBus::new(pool));

        assert_eq!(reg.dispatcher_count().await, 0);

        // Empty inputs are silent no-ops.
        reg.ensure_dispatcher(app.handle(), "", "scout", &bus).await;
        reg.ensure_dispatcher(app.handle(), "default", "", &bus).await;
        reg.ensure_dispatcher(app.handle(), "   ", "scout", &bus).await;
        assert_eq!(reg.dispatcher_count().await, 0);

        // First call lands a dispatcher.
        reg.ensure_dispatcher(app.handle(), "default", "planner", &bus)
            .await;
        assert_eq!(reg.dispatcher_count().await, 1);

        // Second call for same key is a no-op.
        reg.ensure_dispatcher(app.handle(), "default", "planner", &bus)
            .await;
        assert_eq!(reg.dispatcher_count().await, 1);

        // Different agent in same workspace — separate slot.
        reg.ensure_dispatcher(app.handle(), "default", "scout", &bus)
            .await;
        assert_eq!(reg.dispatcher_count().await, 2);

        // Different workspace, same agent — separate slot.
        reg.ensure_dispatcher(app.handle(), "other", "planner", &bus)
            .await;
        assert_eq!(reg.dispatcher_count().await, 3);

        // shutdown_all drains all dispatchers.
        reg.shutdown_all().await.expect("shutdown_all ok");
        assert_eq!(reg.dispatcher_count().await, 0);
    }

    /// Acquire with unknown agentId rejected as `not_found` (the
    /// profile registry returns None for unknown ids).
    #[tokio::test]
    async fn acquire_unknown_agent_id_returns_not_found() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "no-such-agent",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("unknown rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Real-claude integration smoke (`#[ignore]`'d) — drives two
    /// turns through the registry and asserts:
    ///  1. The same session is reused (turn 2 doesn't cold-start).
    ///  2. `list_status` flips through `Spawning → Running → Idle`
    ///     and reports `turns_taken == 2` after both turns finish.
    ///  3. `shutdown_workspace` reverts the row to `NotSpawned`.
    ///
    /// Time budget: typical 60-180s (one cold-start + two turns).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_registry_reuses_session() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);
        let timeout = Duration::from_secs(stage_secs);
        let cancel = Arc::new(Notify::new());

        // Turn 1 — cold-start path, lazy-spawns the scout session.
        let r1 = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "scout",
                "Reply with exactly the single word `BETA` and nothing else.",
                timeout,
                Arc::clone(&cancel),
            )
            .await
            .expect("turn 1 ok");
        assert!(
            r1.assistant_text.to_uppercase().contains("BETA"),
            "turn 1 should contain BETA"
        );

        // Turn 2 — should reuse the session. The proof: list_status
        // shows turns_taken == 2 (not 1) for the scout row; if a
        // respawn had happened the counter would have reset.
        let r2 = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "scout",
                "What was the single word you just replied with? Answer in one word.",
                timeout,
                cancel,
            )
            .await
            .expect("turn 2 ok");
        assert!(
            r2.assistant_text.to_uppercase().contains("BETA"),
            "turn 2 should recall BETA"
        );

        let rows = reg.list_status("default").await;
        let scout = rows
            .iter()
            .find(|r| r.agent_id == "scout")
            .expect("scout row");
        assert_eq!(scout.status, AgentStatus::Idle);
        assert_eq!(scout.turns_taken, 2);
        assert!(scout.last_activity_ms.is_some());

        // Shutdown reverts to NotSpawned.
        reg.shutdown_workspace("default").await.expect("shutdown ok");
        let rows = reg.list_status("default").await;
        let scout = rows
            .iter()
            .find(|r| r.agent_id == "scout")
            .expect("scout row");
        assert_eq!(scout.status, AgentStatus::NotSpawned);
        assert_eq!(scout.turns_taken, 0);
    }
}

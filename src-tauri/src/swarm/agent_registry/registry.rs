//! The stateful `SwarmAgentRegistry`: workspace-scoped session +
//! dispatcher lifecycle. See the module-level docs for the
//! concurrency model.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::mpsc;
use tokio::sync::{Mutex, Notify, RwLock};

use crate::error::AppError;
use crate::swarm::persistent_session::{PersistentSession, TurnStreamEvent};
use crate::swarm::profile::ProfileRegistry;
use crate::swarm::transport::InvokeResult;
use crate::time::now_millis;

use super::config::resolve_turn_cap;
use super::event::{agent_event_channel, SwarmAgentEvent};
use super::slot::AgentSlot;
use super::status::{AgentStatus, AgentStatusRow};

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
                Some(slot_arc) => match slot_arc.try_lock() {
                    Ok(slot) => AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: slot.status,
                        turns_taken: slot.turns_taken,
                        last_activity_ms: slot.last_activity_ms,
                    },
                    // CONC-02: the slot is locked → an `invoke_turn` is in
                    // flight. Do NOT block on it here: this runs while
                    // holding the `sessions` RwLock read, and blocking for
                    // a full turn would stall a concurrent new-agent spawn
                    // waiting on the write lock. Report `Running` (accurate
                    // by construction); the next poll reads exact counters
                    // once the turn releases the slot.
                    Err(_) => AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: AgentStatus::Running,
                        turns_taken: 0,
                        last_activity_ms: Some(now_millis()),
                    },
                },
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

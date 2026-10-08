//! Teardown + dispatcher lifecycle: `shutdown_workspace`,
//! `shutdown_all`, and the lazy `ensure_dispatcher`. Split out of
//! [`super`] so the shutdown ordering contract (drain dispatchers
//! before sessions) lives next to the dispatcher spawn it pairs with.

use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::Mutex;

use crate::error::AppError;

use super::super::slot::AgentSlot;
use super::super::status::AgentStatus;
use super::SwarmAgentRegistry;

impl SwarmAgentRegistry {
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
}

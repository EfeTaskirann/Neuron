//! `MailboxAgentDispatcher` — per-(workspace, agent) task that
//! consumes `MailboxEvent::TaskDispatch` events from the W5-01
//! `MailboxBus` and routes matching ones to
//! `SwarmAgentRegistry::acquire_and_invoke_turn` (WP-W5-02).
//!
//! ## Responsibilities
//!
//! 1. **Route** — match incoming `task_dispatch` events against
//!    `target == agent:<this_agent_id>`; ignore events whose target
//!    points at a different agent (or whose `target` lacks the
//!    `agent:` prefix entirely — the bus is single-namespace today
//!    but we are defensive about wire shapes).
//! 2. **Invoke** — call
//!    `SwarmAgentRegistry::acquire_and_invoke_turn` with a fresh
//!    per-invoke cancel `Notify`. When the dispatch sets
//!    `with_help_loop: true` (W5-03), the dispatcher additionally
//!    parses the specialist's `assistant_text` for a
//!    `neuron_help` block (W4-05 substrate); on hit, it emits
//!    `MailboxEvent::AgentHelpRequest`, awaits a matching
//!    `MailboxEvent::CoordinatorHelpOutcome` (filter by
//!    `target_agent_id`), and feeds the outcome back to the same
//!    specialist session as the next turn's user message. The
//!    loop is bounded by `MAX_HELP_ROUNDS` (3, the same cap the
//!    deleted `RegistryTransport` used in W4-06).
//! 3. **Emit** — on every result (success OR failure) the dispatcher
//!    writes back a `MailboxEvent::AgentResult` whose envelope's
//!    `parent_id` points at the dispatch row's autoincrement `id`.
//!    This keeps the projector (W5-04) seeing a uniform reply-to
//!    chain regardless of error path — failures land as
//!    `assistant_text: "error: <msg>"`, `total_cost_usd: 0.0`,
//!    `turn_count: 0`.
//!
//! ## Cancel handling
//!
//! On `MailboxEvent::JobCancel { job_id }` the dispatcher inspects
//! its `current_invoke` slot. If a turn is in flight for the same
//! `job_id`, the slot's `Arc<Notify>` is signalled (`notify_one`) so
//! `PersistentSession::invoke_turn` returns `AppError::Cancelled`
//! gracefully (W4-01 cancel contract). The race between the lookup
//! and the main loop clearing the slot post-result is benign: a
//! late-arriving cancel against an already-finished turn is a no-op
//! `notify_one` against a `Notify` nobody is listening on.
//!
//! ## Lagged receiver
//!
//! When the broadcast channel overflows (`BROADCAST_CAPACITY` = 64
//! in `MailboxBus`), `recv` returns `RecvError::Lagged(n)`. The
//! dispatcher logs `tracing::warn!` with the skipped count and
//! continues — the SQL log is the source of truth, and any
//! dispatch the dispatcher missed in the lag burst is still
//! recoverable post-hoc by the W5-04 projector replay path.
//!
//! ## Test design (per WP §"Mocking the registry")
//!
//! The unit tests exercise the dispatcher against the real
//! `MailboxBus` but with a closure-based mock of the registry's
//! invoke surface. We abstract that surface into a tiny trait
//! [`AgentInvoker`] (one method, returning `impl Future` so we
//! match the existing W3-12 `Transport` trait pattern and avoid
//! pulling in `async-trait` per Charter §"no new deps"). Production
//! wiring uses [`SwarmAgentRegistryInvoker`] which delegates to the
//! real method; tests use `MockAgentInvoker` (defined under
//! [`tests`]) with a closure-based return value. The trait surface
//! is tiny so the abstraction cost is one extra type and one impl.
//!
//! ## Module layout
//!
//! This file owns the public dispatcher handle ([`MailboxAgentDispatcher`])
//! and its cancel slot; the rest is split into focused submodules:
//!
//! - [`config`] — dispatch timeout + help-loop bounds.
//! - [`invoker`] — the [`AgentInvoker`] seam + production impl.
//! - [`routing`] — `parse_agent_target`, the main `select!` loop, and
//!   per-envelope routing.
//! - [`invoke`] — the invoke + emit cycle, including the W5-03 help loop.

use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope};

mod config;
mod invoke;
mod invoker;
mod routing;
#[cfg(test)]
mod tests;

pub use invoker::{AgentInvoker, SwarmAgentRegistryInvoker};
pub use routing::parse_agent_target;

use routing::run_loop;

// ---------------------------------------------------------------------
// MailboxAgentDispatcher
// ---------------------------------------------------------------------

/// One dispatcher task per `(workspace_id, agent_id)`. Owns:
/// - a `JoinHandle<()>` for the main loop
/// - a shutdown `Notify` the main loop selects on
/// - the `current_invoke` slot used by the cancel branch
pub struct MailboxAgentDispatcher {
    workspace_id: String,
    agent_id: String,
    handle: JoinHandle<()>,
    shutdown: Arc<Notify>,
    current_invoke: Arc<Mutex<Option<InvokeSlot>>>,
}

#[derive(Clone)]
pub(super) struct InvokeSlot {
    pub(super) job_id: String,
    pub(super) cancel: Arc<Notify>,
}

impl MailboxAgentDispatcher {
    /// Spawn a dispatcher subscribed to `bus` for `workspace_id`,
    /// routing `agent:<agent_id>` dispatches to `invoker`.
    ///
    /// Production callers pass a
    /// [`SwarmAgentRegistryInvoker`] wrapping the live registry +
    /// app handle; tests pass a `MockAgentInvoker`.
    pub async fn spawn<R: Runtime, I: AgentInvoker>(
        app: AppHandle<R>,
        workspace_id: String,
        agent_id: String,
        invoker: Arc<I>,
        bus: Arc<MailboxBus>,
    ) -> Self {
        let receiver = bus.subscribe(&workspace_id).await;
        Self::spawn_with_receiver(
            app,
            workspace_id,
            agent_id,
            invoker,
            bus,
            receiver,
        )
    }

    /// Spawn helper that takes an already-owned receiver. Used by
    /// tests (and by `ensure_dispatcher` if a test wants a custom
    /// receiver).
    pub fn spawn_with_receiver<R: Runtime, I: AgentInvoker>(
        app: AppHandle<R>,
        workspace_id: String,
        agent_id: String,
        invoker: Arc<I>,
        bus: Arc<MailboxBus>,
        receiver: broadcast::Receiver<MailboxEnvelope>,
    ) -> Self {
        let shutdown = Arc::new(Notify::new());
        let current_invoke: Arc<Mutex<Option<InvokeSlot>>> =
            Arc::new(Mutex::new(None));

        let workspace_id_for_loop = workspace_id.clone();
        let agent_id_for_loop = agent_id.clone();
        let shutdown_for_loop = Arc::clone(&shutdown);
        let current_invoke_for_loop = Arc::clone(&current_invoke);
        let invoker_for_loop = Arc::clone(&invoker);
        let bus_for_loop = Arc::clone(&bus);
        let app_for_loop = app.clone();

        let handle = tokio::spawn(async move {
            run_loop(
                app_for_loop,
                workspace_id_for_loop,
                agent_id_for_loop,
                invoker_for_loop,
                bus_for_loop,
                receiver,
                shutdown_for_loop,
                current_invoke_for_loop,
            )
            .await;
        });

        Self {
            workspace_id,
            agent_id,
            handle,
            shutdown,
            current_invoke,
        }
    }

    /// Diagnostics — the workspace this dispatcher is bound to.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Diagnostics — the agent this dispatcher is bound to.
    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Drain the dispatcher: signal the main loop to break out of
    /// `select!` and await the join handle. Idempotent — calling
    /// twice is a no-op (`Notify::notify_one` after the loop has
    /// already exited just sets a permit nobody consumes).
    ///
    /// If a turn is in flight when shutdown is called, the
    /// in-flight invoke is also cancelled so `acquire_and_invoke_turn`
    /// returns promptly rather than running to completion.
    pub async fn shutdown(self) {
        // Cancel any in-flight invoke so the loop doesn't block
        // on a multi-second `claude` call before noticing the
        // shutdown signal.
        {
            let slot = self.current_invoke.lock().await;
            if let Some(s) = slot.as_ref() {
                s.cancel.notify_one();
            }
        }
        self.shutdown.notify_one();
        // Best-effort join; a panic in the loop shouldn't block
        // app shutdown.
        let _ = self.handle.await;
    }
}

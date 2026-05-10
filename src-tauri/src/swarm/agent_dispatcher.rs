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
//!    per-invoke cancel `Notify`. The non-help variant is used
//!    deliberately: help-loop migration is W5-03 scope (the
//!    Coordinator brain WP), so today's W4-05 substrate stays
//!    transparent for FSM callers; the dispatcher path simply
//!    surfaces `neuron_help` text as plain `assistant_text` and the
//!    W5-03 brain parses it client-side.
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
//! `#[cfg(test)] mod tests` further down) with a closure-based
//! return value. The trait surface is tiny so the abstraction
//! cost is one extra type and one impl.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::mailbox_bus::{
    MailboxBus, MailboxEnvelope, MailboxEvent,
};
use crate::swarm::transport::InvokeResult;

/// Default per-invoke timeout. Mirrors
/// `commands::swarm::stage_timeout()` (60s default; env override
/// `NEURON_SWARM_STAGE_TIMEOUT_SEC`). Re-implemented here rather
/// than re-exported from the commands module to avoid a swarm →
/// commands cycle.
const DEFAULT_DISPATCH_TIMEOUT_SECS: u64 = 60;
const STAGE_TIMEOUT_ENV: &str = "NEURON_SWARM_STAGE_TIMEOUT_SEC";

fn dispatch_timeout() -> Duration {
    match std::env::var(STAGE_TIMEOUT_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>()
        {
            Ok(0) => {
                tracing::warn!(
                    %STAGE_TIMEOUT_ENV,
                    "value `0` is not a valid stage timeout; \
                     falling back to default in dispatcher"
                );
                Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS)
            }
            Ok(secs) => Duration::from_secs(secs),
            Err(e) => {
                tracing::warn!(
                    %STAGE_TIMEOUT_ENV,
                    raw = %raw,
                    error = %e,
                    "stage timeout override is not a non-negative \
                     integer; using default in dispatcher"
                );
                Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS)
            }
        },
        _ => Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS),
    }
}

/// Strip the `agent:` prefix from a `MailboxEvent::TaskDispatch`
/// `target`. Returns `Some(<id>)` for `agent:<id>` (with non-empty
/// id), `None` otherwise. The agent-id is whatever follows the
/// `agent:` prefix verbatim — no further validation; the registry's
/// own `acquire_and_invoke_turn` rejects empty / whitespace ids
/// downstream so a malformed value lands as `InvalidInput` not a
/// silent skip.
pub fn parse_agent_target(target: &str) -> Option<&str> {
    let stripped = target.strip_prefix("agent:")?;
    if stripped.is_empty() {
        None
    } else {
        Some(stripped)
    }
}

// ---------------------------------------------------------------------
// AgentInvoker — small trait the dispatcher depends on.
// ---------------------------------------------------------------------

/// Test-injection seam over `SwarmAgentRegistry::acquire_and_invoke_turn`.
/// One method; production impl delegates straight through, mock
/// impls return canned `InvokeResult`s without spawning `claude`.
///
/// Same pattern as `swarm::transport::Transport`: returns
/// `impl Future` (stable since 1.75) instead of `async fn` so we
/// don't need `async-trait` (Charter §"no new deps").
///
/// `Send + Sync + 'static` so the dispatcher can spawn a tokio task
/// holding an `Arc<I>` without lifetime juggling.
pub trait AgentInvoker: Send + Sync + 'static {
    /// Invoke one turn against the named (workspace, agent). Cancel
    /// is forwarded to the underlying session's
    /// `PersistentSession::invoke_turn`.
    fn invoke_turn(
        &self,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send;
}

/// Production impl: forwards to
/// `SwarmAgentRegistry::acquire_and_invoke_turn` (no help loop —
/// help loop is W5-03 scope).
pub struct SwarmAgentRegistryInvoker<R: Runtime> {
    app: AppHandle<R>,
    registry: Arc<SwarmAgentRegistry>,
}

impl<R: Runtime> SwarmAgentRegistryInvoker<R> {
    pub fn new(
        app: AppHandle<R>,
        registry: Arc<SwarmAgentRegistry>,
    ) -> Self {
        Self { app, registry }
    }
}

impl<R: Runtime> AgentInvoker for SwarmAgentRegistryInvoker<R> {
    fn invoke_turn(
        &self,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send {
        let registry = Arc::clone(&self.registry);
        let app = self.app.clone();
        let workspace_id = workspace_id.to_string();
        let agent_id = agent_id.to_string();
        let user_message = user_message.to_string();
        async move {
            registry
                .acquire_and_invoke_turn(
                    &app,
                    &workspace_id,
                    &agent_id,
                    &user_message,
                    timeout,
                    cancel,
                )
                .await
        }
    }
}

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
struct InvokeSlot {
    job_id: String,
    cancel: Arc<Notify>,
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

// ---------------------------------------------------------------------
// Main loop body — extracted so `tokio::spawn` doesn't need to
// inline the whole select.
// ---------------------------------------------------------------------

/// Internal: the main loop spawns a child task per dispatch so the
/// loop can continue draining cancel events while the invoke is in
/// flight. The child task drives invoke + emit and clears the
/// `current_invoke` slot on completion.
#[allow(clippy::too_many_arguments)]
async fn run_loop<R: Runtime, I: AgentInvoker>(
    app: AppHandle<R>,
    workspace_id: String,
    agent_id: String,
    invoker: Arc<I>,
    bus: Arc<MailboxBus>,
    mut receiver: broadcast::Receiver<MailboxEnvelope>,
    shutdown: Arc<Notify>,
    current_invoke: Arc<Mutex<Option<InvokeSlot>>>,
) {
    // We track outstanding invoke tasks so we can join them on
    // shutdown. Per WP §"Out of scope (multi-job-per-workspace)"
    // a single agent only sees one in-flight invoke at a time
    // today (the IPC + W5-05 lock enforces that), but we don't
    // assume that here — multiple TaskDispatch events targeting
    // this agent will fan out into parallel child tasks. The
    // `current_invoke` slot only tracks the *most recent* dispatch
    // for cancel routing (W5-05 hardens this).
    let mut invoke_tasks: Vec<JoinHandle<()>> = Vec::new();

    loop {
        // Reap any finished child tasks so the Vec doesn't grow
        // unbounded on a long-lived dispatcher.
        invoke_tasks.retain(|h| !h.is_finished());

        tokio::select! {
            biased;
            // Shutdown wins over event delivery — explicit so app
            // close drains promptly even when a burst of events is
            // queued.
            _ = shutdown.notified() => {
                tracing::debug!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    "agent dispatcher: shutdown signal received"
                );
                break;
            }
            recv_result = receiver.recv() => {
                match recv_result {
                    Ok(envelope) => {
                        if let Some(handle) = handle_envelope(
                            &app,
                            &workspace_id,
                            &agent_id,
                            &invoker,
                            &bus,
                            &current_invoke,
                            envelope,
                        ).await {
                            invoke_tasks.push(handle);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            workspace_id = %workspace_id,
                            agent_id = %agent_id,
                            skipped = skipped,
                            "agent dispatcher: broadcast receiver lagged; \
                             SQL log is source of truth — events skipped \
                             will surface via projector replay"
                        );
                        // Continue — the receiver auto-resyncs to the
                        // newest event after a lag.
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!(
                            workspace_id = %workspace_id,
                            agent_id = %agent_id,
                            "agent dispatcher: broadcast channel closed; \
                             exiting loop"
                        );
                        break;
                    }
                }
            }
        }
    }

    // Drain outstanding invoke tasks — shutdown should have
    // signalled cancel on the in-flight slot, so each task's
    // invoker.invoke_turn returns promptly. Best-effort: if any
    // panicked, ignore the join error.
    for handle in invoke_tasks {
        let _ = handle.await;
    }
}

/// Returns `Some(JoinHandle)` if a TaskDispatch was routed for
/// this dispatcher (so the caller can track it for graceful
/// drain on shutdown); `None` for all other event kinds and
/// for non-matching dispatches.
#[allow(clippy::too_many_arguments)]
async fn handle_envelope<R: Runtime, I: AgentInvoker>(
    app: &AppHandle<R>,
    workspace_id: &str,
    agent_id: &str,
    invoker: &Arc<I>,
    bus: &Arc<MailboxBus>,
    current_invoke: &Arc<Mutex<Option<InvokeSlot>>>,
    envelope: MailboxEnvelope,
) -> Option<JoinHandle<()>> {
    match &envelope.event {
        MailboxEvent::TaskDispatch {
            job_id,
            target,
            prompt,
            with_help_loop: _, // help-loop is W5-03 scope; W5-02
                                // calls the non-help variant
                                // unconditionally.
        } => {
            // Route — does this dispatch target *us*?
            let target_id = match parse_agent_target(target) {
                Some(id) => id,
                None => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        target = %target,
                        "agent dispatcher: dispatch target lacks \
                         `agent:` prefix; ignoring"
                    );
                    return None;
                }
            };
            if target_id != agent_id {
                tracing::trace!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    target = %target,
                    "agent dispatcher: dispatch targets a different \
                     agent; ignoring"
                );
                return None;
            }

            // Set up the invoke slot so a later `JobCancel` can
            // reach the in-flight Notify.
            let cancel = Arc::new(Notify::new());
            {
                let mut slot = current_invoke.lock().await;
                *slot = Some(InvokeSlot {
                    job_id: job_id.clone(),
                    cancel: Arc::clone(&cancel),
                });
            }

            let job_id_for_emit = job_id.clone();
            let prompt_for_invoke = prompt.clone();
            let dispatch_id = envelope.id;

            tracing::debug!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                job_id = %job_id_for_emit,
                dispatch_id,
                "agent dispatcher: spawning invoke task"
            );

            // Spawn the invoke into its own task so the main loop
            // can continue selecting on cancel events.
            let invoker_for_task = Arc::clone(invoker);
            let bus_for_task = Arc::clone(bus);
            let current_invoke_for_task = Arc::clone(current_invoke);
            let app_for_task = app.clone();
            let workspace_id_for_task = workspace_id.to_string();
            let agent_id_for_task = agent_id.to_string();
            let job_id_for_task = job_id_for_emit.clone();

            let handle = tokio::spawn(async move {
                drive_invoke(
                    app_for_task,
                    workspace_id_for_task,
                    agent_id_for_task,
                    job_id_for_task,
                    prompt_for_invoke,
                    dispatch_id,
                    cancel,
                    invoker_for_task,
                    bus_for_task,
                    current_invoke_for_task,
                )
                .await;
            });
            Some(handle)
        }
        MailboxEvent::JobCancel { job_id } => {
            // Look up the in-flight slot. Race window between this
            // lock and the invoke task clearing the slot is benign
            // (documented in module docs).
            let slot = current_invoke.lock().await;
            match slot.as_ref() {
                Some(s) if s.job_id == *job_id => {
                    tracing::debug!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        job_id = %job_id,
                        "agent dispatcher: cancelling in-flight turn"
                    );
                    s.cancel.notify_one();
                }
                Some(other) => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        cancel_job = %job_id,
                        in_flight_job = %other.job_id,
                        "agent dispatcher: JobCancel for a different \
                         job; ignoring"
                    );
                }
                None => {
                    tracing::trace!(
                        workspace_id = %workspace_id,
                        agent_id = %agent_id,
                        job_id = %job_id,
                        "agent dispatcher: JobCancel arrived but no \
                         turn is in flight; ignoring"
                    );
                }
            }
            None
        }
        // Other event kinds are not the dispatcher's concern.
        _ => {
            tracing::trace!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                kind = %envelope.event.kind_str(),
                "agent dispatcher: ignoring non-dispatch event kind"
            );
            None
        }
    }
}

/// Run one invoke + emit cycle. Spawned as a child of the main
/// loop so the loop can continue selecting on cancel events. The
/// `current_invoke` slot is cleared at the end so future cancels
/// don't land on a stale Notify.
#[allow(clippy::too_many_arguments)]
async fn drive_invoke<R: Runtime, I: AgentInvoker>(
    app: AppHandle<R>,
    workspace_id: String,
    agent_id: String,
    job_id: String,
    prompt: String,
    dispatch_id: i64,
    cancel: Arc<Notify>,
    invoker: Arc<I>,
    bus: Arc<MailboxBus>,
    current_invoke: Arc<Mutex<Option<InvokeSlot>>>,
) {
    let invoke_result = invoker
        .invoke_turn(
            &workspace_id,
            &agent_id,
            &prompt,
            dispatch_timeout(),
            Arc::clone(&cancel),
        )
        .await;

    // Clear the slot BEFORE emitting the result so a concurrent
    // JobCancel doesn't land a notify_one on a Notify nobody is
    // listening on. (Benign even if it does — Notify::notify_one
    // without a waiter just sets a permit consumed by the next
    // .notified().) We compare-and-swap on the job_id so a *later*
    // dispatch (from a multi-job future) doesn't accidentally have
    // its slot cleared by an earlier task's completion.
    {
        let mut slot = current_invoke.lock().await;
        if let Some(s) = slot.as_ref() {
            if s.job_id == job_id {
                *slot = None;
            }
        }
    }

    // AgentResult ALWAYS emitted — even on invoke failure. Failures
    // land as `assistant_text: "error: <msg>"`, `total_cost_usd:
    // 0.0`, `turn_count: 0`. Keeps the projector stream uniform.
    let (assistant_text, total_cost_usd, turn_count) = match invoke_result {
        Ok(result) => (
            result.assistant_text,
            result.total_cost_usd,
            result.turn_count,
        ),
        Err(err) => {
            tracing::warn!(
                workspace_id = %workspace_id,
                agent_id = %agent_id,
                job_id = %job_id,
                error = %err.message(),
                "agent dispatcher: invoke failed; emitting \
                 error AgentResult"
            );
            (
                format!("error: {}", err.message()),
                0.0_f64,
                0_u32,
            )
        }
    };

    let summary = format!(
        "agent {agent_id} result for job {job_id} \
         ({turn_count} turns, ${total_cost_usd:.4})"
    );
    let from_pane = format!("agent:{agent_id}");
    let to_pane = "agent:coordinator".to_string();
    let event_emit = MailboxEvent::AgentResult {
        job_id: job_id.clone(),
        agent_id: agent_id.clone(),
        assistant_text,
        total_cost_usd,
        turn_count,
    };
    if let Err(e) = bus
        .emit_typed(
            &app,
            &workspace_id,
            &from_pane,
            &to_pane,
            &summary,
            Some(dispatch_id),
            event_emit,
        )
        .await
    {
        tracing::warn!(
            workspace_id = %workspace_id,
            agent_id = %agent_id,
            error = %e.message(),
            "agent dispatcher: failed to emit AgentResult; \
             SQL log is source of truth, projector replay can \
             still see the dispatch row"
        );
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration as StdDuration;
    use tokio::time::{sleep, timeout};

    // ----------------------------------------------------------------
    // Mock invoker — closure-based stub that records every call and
    // returns a canned result (or signals an error). The
    // `wait_for_cancel` flag holds the call until the supplied
    // Notify fires, simulating a long-running `claude` turn that the
    // dispatcher's JobCancel branch can interrupt.
    // ----------------------------------------------------------------

    #[derive(Clone)]
    struct InvokeCall {
        workspace_id: String,
        agent_id: String,
        user_message: String,
    }

    enum MockBehavior {
        Ok {
            assistant_text: String,
            total_cost_usd: f64,
            turn_count: u32,
        },
        Err(String),
        WaitForCancel,
    }

    struct MockAgentInvoker {
        calls: Arc<StdMutex<Vec<InvokeCall>>>,
        behavior: Arc<Mutex<MockBehavior>>,
    }

    impl MockAgentInvoker {
        fn new_ok(text: &str) -> Self {
            Self {
                calls: Arc::new(StdMutex::new(Vec::new())),
                behavior: Arc::new(Mutex::new(MockBehavior::Ok {
                    assistant_text: text.to_string(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                })),
            }
        }

        fn new_err(msg: &str) -> Self {
            Self {
                calls: Arc::new(StdMutex::new(Vec::new())),
                behavior: Arc::new(Mutex::new(MockBehavior::Err(
                    msg.to_string(),
                ))),
            }
        }

        fn new_wait_for_cancel() -> Self {
            Self {
                calls: Arc::new(StdMutex::new(Vec::new())),
                behavior: Arc::new(Mutex::new(MockBehavior::WaitForCancel)),
            }
        }

        fn calls(&self) -> Vec<InvokeCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AgentInvoker for MockAgentInvoker {
        fn invoke_turn(
            &self,
            workspace_id: &str,
            agent_id: &str,
            user_message: &str,
            _timeout: Duration,
            cancel: Arc<Notify>,
        ) -> impl std::future::Future<
            Output = Result<InvokeResult, AppError>,
        > + Send {
            self.calls.lock().unwrap().push(InvokeCall {
                workspace_id: workspace_id.to_string(),
                agent_id: agent_id.to_string(),
                user_message: user_message.to_string(),
            });
            let behavior = Arc::clone(&self.behavior);
            async move {
                let behavior = behavior.lock().await;
                match &*behavior {
                    MockBehavior::Ok {
                        assistant_text,
                        total_cost_usd,
                        turn_count,
                    } => Ok(InvokeResult {
                        session_id: "mock-session".to_string(),
                        assistant_text: assistant_text.clone(),
                        total_cost_usd: *total_cost_usd,
                        turn_count: *turn_count,
                    }),
                    MockBehavior::Err(msg) => {
                        Err(AppError::SwarmInvoke(msg.clone()))
                    }
                    MockBehavior::WaitForCancel => {
                        drop(behavior);
                        cancel.notified().await;
                        Err(AppError::Cancelled(
                            "cancelled by test".into(),
                        ))
                    }
                }
            }
        }
    }

    // ----------------------------------------------------------------
    // parse_agent_target tests
    // ----------------------------------------------------------------

    #[test]
    fn parse_agent_target_strips_prefix() {
        assert_eq!(parse_agent_target("agent:scout"), Some("scout"));
        assert_eq!(
            parse_agent_target("agent:backend-builder"),
            Some("backend-builder")
        );
    }

    #[test]
    fn parse_agent_target_rejects_missing_prefix() {
        assert_eq!(parse_agent_target("scout"), None);
        assert_eq!(parse_agent_target("pane:p1"), None);
        assert_eq!(parse_agent_target(""), None);
        assert_eq!(parse_agent_target("Agent:scout"), None);
    }

    #[test]
    fn parse_agent_target_rejects_empty_id() {
        assert_eq!(parse_agent_target("agent:"), None);
    }

    // ----------------------------------------------------------------
    // Dispatcher routing
    // ----------------------------------------------------------------

    /// Helper: poll the bus's mailbox table for a row matching the
    /// predicate, with a soft timeout so the test fails fast rather
    /// than hanging if the dispatcher never emits.
    async fn wait_for_envelope<F>(
        bus: &MailboxBus,
        kind: &str,
        predicate: F,
    ) -> Option<MailboxEnvelope>
    where
        F: Fn(&MailboxEnvelope) -> bool,
    {
        let deadline =
            std::time::Instant::now() + StdDuration::from_secs(5);
        loop {
            let rows = bus
                .list_typed(Some(kind), None, Some(500))
                .await
                .expect("list ok");
            if let Some(env) =
                rows.into_iter().find(|env| predicate(env))
            {
                return Some(env);
            }
            if std::time::Instant::now() > deadline {
                return None;
            }
            sleep(StdDuration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn dispatcher_routes_matching_target() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker =
            Arc::new(MockAgentInvoker::new_ok("planner says hi"));

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // Dispatch matching target.
        let env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:planner",
                "kick off",
                None,
                MailboxEvent::TaskDispatch {
                    job_id: "j-1".into(),
                    target: "agent:planner".into(),
                    prompt: "Plan the build".into(),
                    with_help_loop: false,
                },
            )
            .await
            .expect("emit dispatch");

        // Wait for the AgentResult emit.
        let result = wait_for_envelope(&bus, "agent_result", |e| {
            e.parent_id == Some(env.id)
        })
        .await
        .expect("agent result emitted");

        match &result.event {
            MailboxEvent::AgentResult {
                job_id,
                agent_id,
                assistant_text,
                turn_count,
                ..
            } => {
                assert_eq!(job_id, "j-1");
                assert_eq!(agent_id, "planner");
                assert_eq!(assistant_text, "planner says hi");
                assert_eq!(*turn_count, 1);
            }
            _ => panic!("unexpected event variant"),
        }

        let calls = invoker.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].workspace_id, "default");
        assert_eq!(calls[0].agent_id, "planner");
        assert_eq!(calls[0].user_message, "Plan the build");

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_ignores_non_matching_target() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker = Arc::new(MockAgentInvoker::new_ok("planner"));

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // Emit dispatches for OTHER agents.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:scout",
            "wrong target",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:scout".into(),
                prompt: "Investigate".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "pane:p1",
            "non-agent prefix",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-2".into(),
                target: "pane:p1".into(),
                prompt: "Hello".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:",
            "empty id",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-3".into(),
                target: "agent:".into(),
                prompt: "Hello".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");

        // Give the dispatcher a moment to process and ignore.
        sleep(StdDuration::from_millis(150)).await;

        // No AgentResult should have been emitted.
        let results =
            bus.list_typed(Some("agent_result"), None, None).await.unwrap();
        assert!(results.is_empty(), "no results expected: {results:?}");
        assert!(invoker.calls().is_empty());

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_emits_agent_result_with_parent_id() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker =
            Arc::new(MockAgentInvoker::new_ok("ok ok"));

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "scout".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        let env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:scout",
                "go",
                None,
                MailboxEvent::TaskDispatch {
                    job_id: "j-42".into(),
                    target: "agent:scout".into(),
                    prompt: "hi".into(),
                    with_help_loop: true,
                },
            )
            .await
            .expect("emit");

        let result = wait_for_envelope(&bus, "agent_result", |e| {
            e.parent_id == Some(env.id)
        })
        .await
        .expect("agent result emitted");

        // parent_id chains back to the dispatch row.
        assert_eq!(result.parent_id, Some(env.id));
        assert_eq!(result.from_pane, "agent:scout");
        assert_eq!(result.to_pane, "agent:coordinator");
        if let MailboxEvent::AgentResult {
            assistant_text,
            total_cost_usd,
            ..
        } = &result.event
        {
            assert_eq!(assistant_text, "ok ok");
            assert!((*total_cost_usd - 0.01).abs() < 1e-9);
        } else {
            panic!("unexpected variant");
        }

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_emits_error_result_on_invoke_failure() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker =
            Arc::new(MockAgentInvoker::new_err("subprocess died"));

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        let env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:planner",
                "go",
                None,
                MailboxEvent::TaskDispatch {
                    job_id: "j-err".into(),
                    target: "agent:planner".into(),
                    prompt: "explode".into(),
                    with_help_loop: false,
                },
            )
            .await
            .expect("emit");

        let result = wait_for_envelope(&bus, "agent_result", |e| {
            e.parent_id == Some(env.id)
        })
        .await
        .expect("agent result emitted");

        match &result.event {
            MailboxEvent::AgentResult {
                assistant_text,
                total_cost_usd,
                turn_count,
                ..
            } => {
                assert!(
                    assistant_text.starts_with("error:"),
                    "unexpected: {assistant_text}"
                );
                assert!(assistant_text.contains("subprocess died"));
                assert_eq!(*total_cost_usd, 0.0_f64);
                assert_eq!(*turn_count, 0);
            }
            _ => panic!("unexpected variant"),
        }

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_cancels_in_flight_invoke_on_job_cancel() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker = Arc::new(MockAgentInvoker::new_wait_for_cancel());

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // Kick off a dispatch — invoke will block on cancel notify.
        let dispatch_env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:planner",
                "go",
                None,
                MailboxEvent::TaskDispatch {
                    job_id: "j-cancel".into(),
                    target: "agent:planner".into(),
                    prompt: "long".into(),
                    with_help_loop: false,
                },
            )
            .await
            .expect("emit");

        // Wait briefly for the invoker to actually be called (so
        // the slot is populated before the cancel races in).
        let deadline =
            std::time::Instant::now() + StdDuration::from_secs(2);
        loop {
            if !invoker.calls().is_empty() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("invoker never called");
            }
            sleep(StdDuration::from_millis(10)).await;
        }

        // Now signal cancel.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:planner",
            "cancel",
            None,
            MailboxEvent::JobCancel {
                job_id: "j-cancel".into(),
            },
        )
        .await
        .expect("emit cancel");

        // The cancelled invoke surfaces an error AgentResult.
        let result = timeout(
            StdDuration::from_secs(5),
            wait_for_envelope(&bus, "agent_result", |e| {
                e.parent_id == Some(dispatch_env.id)
            }),
        )
        .await
        .expect("timeout waiting for cancel result");
        let env = result.expect("agent result emitted");
        match &env.event {
            MailboxEvent::AgentResult {
                assistant_text, ..
            } => {
                assert!(
                    assistant_text.starts_with("error:"),
                    "expected error result on cancel: {assistant_text}"
                );
            }
            _ => panic!("unexpected variant"),
        }

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_ignores_job_cancel_for_other_job() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker = Arc::new(MockAgentInvoker::new_wait_for_cancel());

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // Kick off j-A.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "go",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-A".into(),
                target: "agent:planner".into(),
                prompt: "long".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");

        // Wait for the invoker to be in flight.
        let deadline =
            std::time::Instant::now() + StdDuration::from_secs(2);
        loop {
            if !invoker.calls().is_empty() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("invoker never called");
            }
            sleep(StdDuration::from_millis(10)).await;
        }

        // Cancel a *different* job.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:user",
            "agent:planner",
            "cancel",
            None,
            MailboxEvent::JobCancel {
                job_id: "j-B".into(),
            },
        )
        .await
        .expect("emit cancel");

        // Give the dispatcher a chance to process the cancel; the
        // invoker MUST still be blocked because the cancel didn't
        // match its job_id.
        sleep(StdDuration::from_millis(200)).await;
        let no_results =
            bus.list_typed(Some("agent_result"), None, None).await.unwrap();
        assert!(
            no_results.is_empty(),
            "no agent_result expected (still in flight): {no_results:?}"
        );

        // shutdown drains the in-flight invoke so the test exits
        // promptly.
        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_handles_lagged_receiver() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));

        // Pre-subscribe so emits land on a real receiver. Holding
        // this `extra_rx` makes the channel "active".
        let mut extra_rx = bus.subscribe("default").await;

        let invoker = Arc::new(MockAgentInvoker::new_ok("survived"));

        // The dispatcher subscribes here, then we burn its receive
        // loop with > 64 events all emitted before it gets a chance
        // to drain.
        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // Flood with 200 unrelated notes — well past the
        // BROADCAST_CAPACITY (64). The dispatcher will see
        // RecvError::Lagged and warn.
        for i in 0..200 {
            bus.emit_typed(
                app.handle(),
                "default",
                "agent:noise",
                "agent:noise",
                &format!("flood {i}"),
                None,
                MailboxEvent::Note,
            )
            .await
            .expect("emit note");
        }
        // Drain the secondary receiver so its buffer doesn't keep
        // backpressure (broadcast doesn't actually backpressure on
        // send — slow consumers see Lagged on next recv).
        while extra_rx.try_recv().is_ok() {}

        // Now emit a genuine dispatch. The dispatcher should
        // process it post-lag.
        let dispatch_env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:coordinator",
                "agent:planner",
                "post-lag",
                None,
                MailboxEvent::TaskDispatch {
                    job_id: "j-postlag".into(),
                    target: "agent:planner".into(),
                    prompt: "post-lag prompt".into(),
                    with_help_loop: false,
                },
            )
            .await
            .expect("emit dispatch");

        let result = wait_for_envelope(&bus, "agent_result", |e| {
            e.parent_id == Some(dispatch_env.id)
        })
        .await
        .expect("dispatcher recovered from lag and processed dispatch");
        match &result.event {
            MailboxEvent::AgentResult { assistant_text, .. } => {
                assert_eq!(assistant_text, "survived");
            }
            _ => panic!("unexpected variant"),
        }

        dispatcher.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_shutdown_drains_cleanly() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool.clone()));
        let invoker = Arc::new(MockAgentInvoker::new_ok("ok"));

        let dispatcher = MailboxAgentDispatcher::spawn(
            app.handle().clone(),
            "default".into(),
            "planner".into(),
            Arc::clone(&invoker),
            Arc::clone(&bus),
        )
        .await;

        // shutdown returns within a reasonable time even with no
        // events in flight.
        timeout(StdDuration::from_secs(2), dispatcher.shutdown())
            .await
            .expect("shutdown drained within 2s");

        // The bus is still usable post-shutdown — emits don't panic.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:noise",
            "agent:noise",
            "post shutdown",
            None,
            MailboxEvent::Note,
        )
        .await
        .expect("post-shutdown emit ok");
    }
}

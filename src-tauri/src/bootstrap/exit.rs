//! The `RunEvent::ExitRequested` teardown sequence.
//!
//! WP-W2-04 §"Acceptance criteria": "Sidecar process is killed
//! on app shutdown (no orphan `python` process on next launch)".
//! WP-W2-06 §"Acceptance criteria": "Killing app cleans all
//! child PTY processes (no orphans on next launch)".
//! `RunEvent::ExitRequested` fires before the runtime stops; we
//! drive every supervisor's shutdown path so neither the Python
//! sidecar nor any spawned shell outlives the window. The
//! `kill_on_drop(true)` set on the agent spawn config is a
//! defensive seatbelt; `portable_pty::ChildKiller` is the
//! explicit kill API for terminals.

use tauri::Manager;

use crate::db;
use crate::sidecar;

/// Everything `lib.rs::run().run(|_, ExitRequested| ...)` does, in
/// teardown order. Each step is independently `try_state`-guarded so
/// a partially-initialised app (setup bailed early) still exits
/// cleanly.
pub(super) fn handle_exit_requested(app: &tauri::AppHandle) {
    if let Some(handle) = app.try_state::<sidecar::agent::SidecarHandle>() {
        // `State::inner()` returns `&SidecarHandle`; we clone
        // the cheap `Arc`-backed handle into the async block
        // so `block_on` does not borrow across the closure.
        let cloned = handle.inner().clone();
        tauri::async_runtime::block_on(async move {
            cloned.shutdown().await;
        });
    }
    // Terminal-Swarm — clear the active session BEFORE
    // shutting the TerminalRegistry down. `stop()` kills
    // panes individually via kill_pane; the subsequent
    // `shutdown_all` is a defensive sweep for any pane
    // that wasn't tracked by the swarm-term session.
    if let Some(swarm_term_registry) = app.try_state::<
        std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>,
    >() {
        let cloned = swarm_term_registry.inner().clone();
        let app_handle = app.clone();
        tauri::async_runtime::block_on(async move {
            let _ = cloned.stop(app_handle).await;
        });
    }
    if let Some(registry) = app.try_state::<sidecar::terminal::TerminalRegistry>() {
        let cloned = registry.inner().clone();
        // shutdown_all needs the DB pool to flush each
        // pane's ring buffer to `pane_lines` synchronously
        // before this hook returns — see report.md §K1.
        if let Some(pool) = app.try_state::<db::DbPool>() {
            let pool_cloned = pool.inner().clone();
            tauri::async_runtime::block_on(async move {
                cloned.shutdown_all(&pool_cloned).await;
            });
        }
    }
    // WP-W5-05 — fan-out JobCancel to every in-flight
    // brain-driven job before tearing the agent
    // registry down. The brain's loop + each
    // dispatcher's invoke notify pick the JobCancel up
    // and unwind cleanly; doing this BEFORE
    // `agent_registry.shutdown_all()` gives the
    // dispatchers a chance to break out of their
    // `tokio::select!`'s and finish in-flight `claude`
    // turns instead of getting SIGKILL'd mid-stream.
    // Idempotent — an empty `swarm_jobs` query is a
    // no-op. The fan-out body lives on `MailboxBus` so
    // the unit tests can exercise it without booting
    // the runtime closure.
    if let Some(bus) = app
        .try_state::<std::sync::Arc<crate::swarm::MailboxBus>>()
    {
        let bus = bus.inner().clone();
        let app_handle = app.clone();
        tauri::async_runtime::block_on(async move {
            let _ = bus
                .cancel_in_flight_brain_jobs(&app_handle)
                .await;
        });
    }
    // WP-W4-02 — kill every persistent agent session
    // before the runtime exits. Same kill_on_drop seatbelt
    // as the rest of the supervisors, but explicit
    // shutdown is cleaner so claude exits via stdin EOF
    // (graceful) rather than SIGKILL on the way out.
    if let Some(agent_registry) = app
        .try_state::<std::sync::Arc<crate::swarm::SwarmAgentRegistry>>()
    {
        let cloned = agent_registry.inner().clone();
        tauri::async_runtime::block_on(async move {
            let _ = cloned.shutdown_all().await;
        });
    }
    // WP-W5-04 — drain every projector before exit so
    // the broadcast subscribers don't outlive the
    // runtime. Idempotent — empty registry is a no-op.
    if let Some(projector_registry) = app
        .try_state::<std::sync::Arc<crate::swarm::JobProjectorRegistry>>()
    {
        let cloned = projector_registry.inner().clone();
        tauri::async_runtime::block_on(async move {
            cloned.shutdown_all().await;
        });
    }
}

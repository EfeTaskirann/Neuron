//! File-system based inter-agent IPC.
//!
//! Each pane is an isolated `claude` REPL. When agent A wants to message
//! agent B, A uses its `Write` tool to atomically create
//! `.bridgespace/<session>/inbox/<B>/<id>.json` with shape:
//!
//! ```json
//! {"from":"scout","to":"orchestrator",
//!  "body":"tamam — foo.rs:42",
//!  "task_id":"t-1"}
//! ```
//!
//! A background poll loop ([`watcher::watcher_loop`]) reads every inbox
//! directory once every [`watcher::WATCH_POLL_MS`] ms, validates each
//! file (parse + hierarchy gate), and delivers the body to the target
//! pane's PTY via bracketed paste. After successful delivery the file
//! moves to `processed/<B>/`; denied / malformed files move to
//! `rejected/<B>/` with a `.reason` sidecar, and the SENDER gets a
//! `[Neuron]` notice in its pane so it does not wait for a reply that
//! can never come. Transient delivery failures leave the file in the
//! inbox so the next tick retries: `target_not_ready` / `target_locked`
//! until the message is 60 s / 180 s old, write errors and unparseable
//! JSON for 5 attempts.
//!
//! Crashes while in flight are non-fatal: every file in `inbox/` is
//! re-scanned on the next tick, so a partially delivered route is
//! retried — file-system as durable queue. Atomic-rename file moves
//! preserve the invariant "inbox = not yet delivered, processed =
//! delivered".
//!
//! ## Layout
//!
//! Split from the original single-file `bridge.rs` (T3) along
//! responsibility lines, public surface preserved byte-for-byte via the
//! flat re-exports below:
//!   * [`envelope`]  — the [`Envelope`] wire type + structural validation.
//!   * [`fsops`]     — filesystem layout/queue ops (`prepare_layout`,
//!                     `ensure_gitignore`, `drop_envelope`, file moves,
//!                     attempts sidecar).
//!   * [`delivery`]  — pane-status gate, route-write + timeout, the
//!                     bracketed-paste wire format.
//!   * [`watcher`]   — the poll loop and per-file delivery state machine.
//!   * [`synth`]     — lifecycle-token fanout on coordinator-bound
//!                     deliveries.
//!   * [`notify`]    — `[Neuron]` notices pasted into a sender's pane
//!                     (rejections, unrecognised lifecycle signals).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Notify;

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::lifecycle::LifecycleStore;

mod delivery;
mod envelope;
mod fsops;
mod notify;
mod synth;
mod watcher;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;

pub(crate) use delivery::paste_then_submit;
pub use envelope::Envelope;
pub use fsops::{
    archive_rejected, drop_envelope, ensure_gitignore, prepare_layout,
};

/// Subdirectory of the project root where the per-session bridge tree
/// lives. Cleaned up in `TerminalSwarmRegistry::stop`.
pub const BRIDGE_DIRNAME: &str = ".bridgespace";

/// Returned by [`install`]. Holding it keeps the watcher alive; drop
/// (via [`uninstall`]) cancels it.
pub struct BridgeHandle {
    pub root: PathBuf,
    cancel: Arc<Notify>,
}

/// Spawn the watcher task for the given session and return a handle
/// that uninstall takes ownership of. Layout under `root` must already
/// exist (call [`prepare_layout`] first).
pub fn install<R: Runtime>(
    app: AppHandle<R>,
    root: PathBuf,
    panes_by_agent: HashMap<String, String>,
    ready_panes: Arc<Mutex<HashSet<String>>>,
    lifecycle: Arc<LifecycleStore>,
) -> BridgeHandle {
    let cancel = Arc::new(Notify::new());
    let cancel_for_task = Arc::clone(&cancel);
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let root_for_task = root.clone();
    tauri::async_runtime::spawn(watcher::watcher_loop(
        app,
        registry,
        root_for_task,
        Arc::new(panes_by_agent),
        ready_panes,
        lifecycle,
        cancel_for_task,
    ));
    BridgeHandle { root, cancel }
}

/// Stop the watcher task started by [`install`]. The bridge root is
/// kept; the caller (session lifecycle) is responsible for deletion.
pub fn uninstall(handle: BridgeHandle) {
    // notify_one stores a permit: the watcher spends most of its time
    // inside process_pending (multi-second write timeouts), not parked
    // in notified() — notify_waiters fired in that window would be
    // lost and the watcher would poll the deleted root forever.
    handle.cancel.notify_one();
}

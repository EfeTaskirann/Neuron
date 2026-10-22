//! File-system based inter-agent IPC.
//!
//! Each pane is an isolated `claude` REPL. When agent A wants to message
//! agent B, A uses its `Write` tool to atomically create a file in its
//! OWN outbox, `.bridgespace/<session>/outbox/<A>/<id>.json`:
//!
//! ```json
//! {"to":"orchestrator",
//!  "body":"tamam — foo.rs:42",
//!  "task_id":"t-1"}
//! ```
//!
//! The sender is the outbox directory; an optional `from` must match it.
//! A background poll loop ([`watcher::watcher_loop`]) reads every outbox
//! once every [`watcher::WATCH_POLL_MS`] ms, validates each
//! file (file gate + parse + hierarchy gate), and delivers the body to the target
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
//! ## Trust model
//!
//! The outbox-derived sender and the hierarchy gate keep the swarm
//! ORDERLY — they stop accidental and simple impersonation — but they are
//! not a security boundary: all nine panes run as the same OS user with
//! the same file access, so an agent can always write into another
//! agent's outbox. Hostile content is contained instead at the paste
//! layer: bodies are stripped of every control character and the
//! bridge's own markers are defused before delivery (see
//! [`delivery::sanitize_for_paste`]), and inbox entries pass a file gate
//! (regular file, ≤ 256 KB) before they are read.
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
//!   * [`stall`]     — reminders for tasks stuck in `Building` /
//!                     `AwaitingReview` (escalated to the coordinator).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::Notify;

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::lifecycle::LifecycleStore;

mod delivery;
mod envelope;
mod fsops;
mod notify;
mod stall;
mod synth;
mod watcher;

#[cfg(test)]
mod e2e_tests;
#[cfg(test)]
mod tests;

pub(crate) use delivery::{paste_then_submit, sanitize_for_paste};
pub(crate) use notify::notify_agent;
pub(crate) use synth::enqueue_fanout;
pub(crate) use watcher::emit_notice;
pub use envelope::Envelope;
pub use fsops::{
    archive_rejected, drop_envelope, ensure_gitignore, prepare_layout,
    sweep_orphan_sessions,
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

/// Agent id → pane id, shared with the session: a restarted agent runs
/// in a new pane, and the watcher reads the current map every tick.
pub type SharedPanes = Arc<RwLock<HashMap<String, String>>>;

/// Why the bridge holds every delivery (see [`BridgeControl`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PauseReason {
    /// The account's usage limit is hit: no agent can answer.
    UsageLimit,
    /// The agents used up the session's message budget.
    Budget,
    /// The user paused the swarm.
    User,
}

/// Holds deliveries while any reason is set. Messages stay in their
/// outboxes in order and go out once the last reason is cleared; the
/// stall clocks are paused alongside by the caller.
#[derive(Debug, Default)]
pub struct BridgeControl {
    paused_by: Mutex<BTreeSet<PauseReason>>,
}

impl BridgeControl {
    /// Set or clear `reason`; true when the bridge's paused state
    /// changed (paused ⇄ running).
    pub fn set_paused(&self, reason: PauseReason, on: bool) -> bool {
        let mut g = self.paused_by.lock().unwrap_or_else(|p| p.into_inner());
        let was_paused = !g.is_empty();
        if on {
            g.insert(reason);
        } else {
            g.remove(&reason);
        }
        let paused = !g.is_empty();
        was_paused != paused
    }

    pub fn is_paused(&self) -> bool {
        !self.paused_by.lock().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    pub fn paused_by(&self, reason: PauseReason) -> bool {
        self.paused_by.lock().unwrap_or_else(|p| p.into_inner()).contains(&reason)
    }
}

/// Spawn the watcher task for the given session and return a handle
/// that uninstall takes ownership of. Layout under `root` must already
/// exist (call [`prepare_layout`] first).
pub fn install<R: Runtime>(
    app: AppHandle<R>,
    root: PathBuf,
    panes_by_agent: SharedPanes,
    ready_panes: Arc<Mutex<HashSet<String>>>,
    lifecycle: Arc<LifecycleStore>,
    control: Arc<BridgeControl>,
) -> BridgeHandle {
    let cancel = Arc::new(Notify::new());
    let cancel_for_task = Arc::clone(&cancel);
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let root_for_task = root.clone();
    tauri::async_runtime::spawn(watcher::watcher_loop(
        app,
        registry,
        root_for_task,
        panes_by_agent,
        ready_panes,
        lifecycle,
        control,
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

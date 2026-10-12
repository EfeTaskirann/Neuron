//! LangGraph Python sidecar supervisor (WP-W2-04).
//!
//! Owns the lifecycle of the `python -m agent_runtime` child process
//! and drives the JSON-RPC frame protocol described in
//! `src-tauri/sidecar/agent_runtime/README.md`.
//!
//! Wiring at a glance:
//!
//! ```text
//! lib.rs::run().setup(...)
//!     ├── db::init                  → SqlitePool managed in app state
//!     └── sidecar::agent::spawn_runtime
//!             → tokio::process::Command(python -m agent_runtime)
//!             → spawns the read loop (stdout → DB writes + Tauri events)
//!             → returns SidecarHandle, managed in app state
//!
//! commands/runs.rs::runs_create
//!     ├── inserts the row with status='running'
//!     └── sidecar::agent::start_run
//!             → writes a `run.start` frame to the child's stdin
//! ```
//!
//! Per WP-W2-04 §"Out of scope":
//!
//!   Cancel signal mid-LLM-call (best effort: kill the sidecar's run
//!   task; do NOT kill the whole sidecar).
//!
//! We expose `start_run` as a thin "post one frame" API. Future WPs
//! that add cancel propagation can add a `cancel_run(run_id)` frame
//! without changing the supervisor's read loop.
//!
//! ## Module layout
//!
//! Split from the original single `agent.rs` along responsibility axes;
//! public surface (`SidecarHandle`, `spawn_runtime`) is preserved by the
//! re-export below so consumers keep using `crate::sidecar::agent::{…}`.
//!
//! - `mod` (this file)  — the stateful `SidecarHandle` + its public API
//!   (`start_run` / `shutdown` / the private `send` frame writer).
//! - `wire`   — inbound/outbound wire shapes (`WireSpan`, `SidecarEvent`,
//!   `RunSpanPayload`, `SerializableWireSpan`).
//! - `spawn`  — the `spawn_runtime` entry point + `resolve_python`.
//! - `reader` — the stdout read loop → DB writer + Tauri event emitter.

mod reader;
mod spawn;
mod wire;

#[cfg(test)]
mod tests;

pub use spawn::spawn_runtime;

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::sidecar::framing::write_frame;
use crate::tuning::SHUTDOWN_GRACE;

// --------------------------------------------------------------------- //
// Public handle managed by Tauri state                                   //
// --------------------------------------------------------------------- //

/// Type-erased handle the rest of the codebase passes around. Every
/// public method consumes `&self` and dispatches into the inner
/// `Arc<Inner>` so cloning is cheap and `tauri::State` can hand out
/// shared references without lock contention on the outer struct.
#[derive(Clone)]
pub struct SidecarHandle {
    inner: Arc<Inner>,
}

pub(super) struct Inner {
    /// Locked write side of the child's stdin. Frames are serialized
    /// here by acquiring the mutex, which prevents two concurrent
    /// `start_run` calls from interleaving bytes.
    stdin: Mutex<Option<ChildStdin>>,
    /// The `Child` itself, kept so `shutdown()` can `kill()` it. We
    /// never poll it for status from this side — the read loop notices
    /// EOF on stdout when the child exits.
    child: Mutex<Option<Child>>,
    /// JoinHandle for the read-loop task spawned at `spawn_runtime`.
    /// Kept so `shutdown()` can `await` it (with a bounded grace) and
    /// drain pending span / `run.completed` frames before the child
    /// is hard-killed. See report.md §K4.
    read_loop: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

// --------------------------------------------------------------------- //
// Public API                                                             //
// --------------------------------------------------------------------- //

impl SidecarHandle {
    /// Build a handle around an already-constructed `Inner`. Used by
    /// `spawn::spawn_runtime` after it has spawned the child and read
    /// loop; kept `pub(super)` so the inner field stays private.
    pub(super) fn from_inner(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// Send a `run.start` frame. Returns `Ok(())` on successful write;
    /// any subsequent failure surfaces as a `run.completed` event with
    /// `status='error'` from the sidecar itself.
    pub async fn start_run(&self, workflow_id: &str, run_id: &str) -> Result<(), AppError> {
        let payload = json!({
            "method": "run.start",
            "params": { "workflowId": workflow_id, "runId": run_id }
        });
        self.send(&payload).await
    }

    /// Tear the sidecar down on app exit. Sends a clean `shutdown`
    /// frame, drops stdin so the Python event loop sees EOF, then
    /// **awaits the read loop with a bounded grace** so any in-flight
    /// `run.completed` / `span.closed` frames land in SQLite before
    /// the child is hard-killed. Idempotent: a second call is a
    /// no-op (slots are already drained).
    ///
    /// See report.md §K4: previously this method called `start_kill`
    /// immediately, dropping pending frames and leaving runs stuck in
    /// `running` after every app close.
    pub async fn shutdown(&self) {
        // 1. Best-effort clean shutdown frame.
        let _ = self.send(&json!({"method": "shutdown"})).await;
        // 2. Drop stdin so Python's blocking `read_in_executor` sees
        //    EOF and the asyncio loop in `__main__.py` exits cleanly,
        //    flushing pending `run.completed` events on its way out.
        {
            let mut stdin_slot = self.inner.stdin.lock().await;
            *stdin_slot = None;
        }
        // 3. Wait for the read-loop task to finish — it returns when
        //    the child closes stdout, which it does once Python's loop
        //    completes. Bound the wait so a wedged child cannot block
        //    app exit indefinitely.
        let read_loop_handle = {
            let mut slot = self.inner.read_loop.lock().await;
            slot.take()
        };
        if let Some(handle) = read_loop_handle {
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, handle).await;
        }
        // 4. Kill if still alive; tolerated for an already-exited
        //    child (Win32 `ERROR_INVALID_PARAMETER`, Unix `ESRCH`).
        let mut child_slot = self.inner.child.lock().await;
        if let Some(mut child) = child_slot.take() {
            let _ = child.start_kill();
        }
    }

    async fn send(&self, value: &Value) -> Result<(), AppError> {
        let body = serde_json::to_vec(value)?;
        let mut guard = self.inner.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| {
            AppError::Sidecar("agent runtime sidecar is not running".into())
        })?;
        write_frame(stdin, &body)
            .await
            .map_err(|e| AppError::Sidecar(format!("write frame: {e}")))?;
        Ok(())
    }
}

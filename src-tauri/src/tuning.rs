//! Centralised tuning constants — one place to adjust timing,
//! buffer sizes, and timeouts. See `tasks/refactor.md` §4 ("Magic
//! timing/ölçek sabitlerinin dağınıklığı") for rationale.
//!
//! Constants used to live in their respective modules
//! (`sidecar/agent.rs`, `sidecar/terminal.rs`, `mcp/client.rs`).
//! Centralising them here makes it possible to tune the whole
//! runtime profile (low-mem device, high-throughput dev) without
//! file-hunting.

use std::time::Duration;

// ----- Sidecar lifecycle -----

/// How long the LangGraph sidecar gets after a clean `shutdown`
/// frame before we issue `start_kill`. Long enough for Python to
/// flush its in-flight `run.completed` events; short enough that
/// the user does not perceive app close as hung.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// Kill grace period — `kill_pane` issues a kill, then verifies the
/// child has exited within this window before declaring success.
/// `portable_pty::ChildKiller::kill` is best-effort; on Windows the
/// ConPTY shuts down asynchronously and we want to give the OS a beat.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// Polling cadence for `try_wait()` in the per-pane waiter task.
/// Lower causes wakeups under quiescent panes, higher delays the
/// `success`/`error` status flip.
pub const WAIT_POLL: Duration = Duration::from_millis(200);

// ----- Terminal ring buffer -----

/// Hard cap on in-memory ring lines per pane. Per WP-W2-06 § "Ring
/// buffer": "5,000 lines per pane. When exceeded, oldest 1,000 dropped".
pub const RING_BUFFER_CAP: usize = 5_000;

/// Number of lines dropped from the front when the ring overflows.
pub const RING_BUFFER_DROP: usize = 1_000;

/// How many of the most recent lines are scanned for awaiting-approval
/// regex matches per output chunk. Per NEURON_TERMINAL_REPORT § state
/// machine: "simple regex on the last 5 lines".
pub const APPROVAL_WINDOW_LINES: usize = 5;

/// Read-buffer size for PTY chunks. 8 KiB is the common pipe buffer
/// size on every host platform; larger values waste memory for typical
/// shell output bursts and smaller values incur extra syscalls.
pub const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Cap on un-flushed pending bytes in the per-pane reader. A child
/// emitting megabytes without a newline (e.g. `tr -d '\n'`-style
/// adversarial output, or progress bars using `\r` only) used to
/// grow `pending` unbounded — see report.md §L8. Force-flush as a
/// single line once it exceeds this cap so memory stays bounded.
pub const MAX_PENDING_BYTES: usize = 1024 * 1024; // 1 MiB

// ----- MCP client -----

/// One MCP request's deadline (initialize, tools/list, tools/call).
/// `npx` cold-start can be slow on Windows (no cache → npm install →
/// server startup), so the budget is deliberately generous.
pub const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

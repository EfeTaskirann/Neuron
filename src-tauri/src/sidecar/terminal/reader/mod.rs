//! Reader + waiter background tasks.
//!
//! The reader task turns raw PTY bytes into `panes:{id}:line` events
//! and ring-buffer lines, drives the awaiting-approval detection, and
//! answers DSR-CPR cursor queries. The waiter task observes child exit,
//! finalises the DB row, and flushes the ring buffer to `pane_lines`.
//!
//! Both run on `spawn_blocking` threads because the underlying
//! `Read` / `Child::try_wait` APIs are synchronous. They mutate the
//! shared `PaneState` under its inner `Mutex`.
//!
//! Split by concern:
//! - [`read_loop`] — the PTY read loop (`run_reader`).
//! - [`transition`] — running / awaiting-approval status transitions.
//! - [`waiter`] — child-exit observation + finalisation (`run_waiter`,
//!   `flush_ring_to_db`).
//! - [`emit`] — the shared line event payload + ring-append helper.

mod emit;
mod read_loop;
mod transition;
mod waiter;

pub(super) use read_loop::run_reader;
pub(super) use waiter::{flush_ring_to_db, run_waiter};

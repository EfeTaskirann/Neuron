//! One-shot `claude` subprocess transport (WP-W3-11 §4).
//!
//! Drives a single user message through the `claude` CLI's stream-json
//! protocol — spawn a per-call child, send one NDJSON user line, await
//! the `result` event, tear the child down. See [`subprocess`] for the
//! full stream-json contract.
//!
//! ## Module layout (refactor, DEEP)
//!
//! This used to be a single ~930-line `transport.rs`. It is now a
//! package that splits the four concerns into sibling submodules and
//! re-exports the public symbols at the same path
//! (`swarm::transport::{InvokeResult, SubprocessTransport, Transport,
//! StreamEvent, classify_event, RingBuffer, …}`) so `swarm::mod`'s
//! `pub use transport::{…}` and every `crate::swarm::transport::*`
//! consumer (`persistent_session`, `agent_dispatcher`,
//! `agent_registry`, `brain`, `commands/swarm`) keep resolving
//! without change:
//!
//! - [`event`] — the pure data types: the `StreamEvent` parser enum
//!   and the public `InvokeResult` output struct.
//! - [`classify`] — the pure, synchronous stream-json line classifier
//!   (`classify_event` + helpers), drivable by tests without a
//!   subprocess.
//! - [`ring`] — the tail-only stderr `RingBuffer` + `fmt_stderr_tail`,
//!   shared with `persistent_session`.
//! - [`subprocess`] — the stateful spawn/drive side: the `Transport`
//!   trait, `SubprocessTransport`, and the `write_persona_tmp` helper.

mod classify;
mod event;
mod ring;
mod subprocess;

#[cfg(test)]
mod tests;

pub use event::InvokeResult;
pub use subprocess::{SubprocessTransport, Transport};

pub(crate) use classify::classify_event;
pub(crate) use event::StreamEvent;
pub(crate) use ring::{fmt_stderr_tail, RingBuffer, STDERR_RING_CAPACITY};
pub(crate) use subprocess::write_persona_tmp;

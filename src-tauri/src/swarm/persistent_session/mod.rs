//! `PersistentSession` — multi-turn `claude` subprocess (WP-W4-01 §1).
//!
//! Sibling to `SubprocessTransport` (W3-11). Same arg builder, same
//! env strip, same stream-json read loop — but the child outlives a
//! single `invoke_turn` call. Stdin stays open between turns so the
//! claude CLI can accept a new `{"type":"user","message":...}` line
//! after each `result` event.
//!
//! Lifecycle owned by W4-02 registry:
//! - `spawn(app, profile)` — exactly once per (workspace, agent)
//! - `invoke_turn(user_message, timeout, cancel)` — repeatable
//! - `shutdown()` — once on workspace close (or on registry-driven
//!   respawn under the turn-cap policy)
//!
//! Thread-safety contract: not `Sync`. The W4-02 registry must
//! serialise access per session — at most one `invoke_turn` in flight
//! at a time. Concurrent turns against the same session are a
//! programming error and would deadlock on the stdin write.
//!
//! Cancel semantics: a fired `Notify` truncates the in-flight turn
//! (returns `AppError::Cancelled`); the child stays alive. Up to a
//! small drain budget after cancel, leftover bytes are read off
//! stdout to preserve framing for the next turn.
//!
//! Out of scope (per WP §"Out of scope"): registry / event channel /
//! help-request parser / FSM integration / specta event types. Those
//! land in W4-02..06.
//!
//! ## Layout (split from the original 991L single file)
//! - `event` — `TurnStreamEvent`, the local hot-path streaming enum
//!   handed to W4-03's per-agent channel.
//! - `session` — the stateful `PersistentSession` (spawn / invoke_turn
//!   / shutdown / Drop + the `attach_stderr_tail` helper).
//! - `read` — the stdout read loop (`read_until_result`), the
//!   post-cancel drain, and the local `InvokeAccum` running state.
//!
//! Public surface re-exported below is byte-identical to the old file:
//! `persistent_session::{PersistentSession, TurnStreamEvent}`.

mod event;
mod read;
mod session;

#[cfg(test)]
mod tests;

pub use event::TurnStreamEvent;
pub use session::PersistentSession;

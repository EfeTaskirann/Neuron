//! WP-W3-12k2 — persistent Orchestrator chat history.
//!
//! Sister module to `orchestrator.rs` (W3-12k1's stateless brain).
//! W3-12k1 shipped a one-shot `swarm:orchestrator_decide` that took a
//! single `user_message` and returned an `OrchestratorOutcome` with no
//! conversation context. W3-12k3 shipped the chat UI panel but kept
//! messages in React state — gone on reload.
//!
//! This module is the SQLite write-through that closes both gaps:
//!
//! 1. The IPC handler (`commands::swarm::swarm_orchestrator_decide`)
//!    persists each user message + each orchestrator outcome through
//!    the helpers below, so reload sees the full thread.
//! 2. The same IPC handler reads the most-recent N messages and
//!    pre-pends them to the prompt via [`render_with_history`], so
//!    the persona sees prior context when deciding the next action.
//! 3. Three new IPCs (`swarm:orchestrator_history`,
//!    `swarm:orchestrator_clear_history`, `swarm:orchestrator_log_job`)
//!    expose the read / clear / job-log surfaces to the frontend.
//!
//! ## Why string-query, not `query!`?
//!
//! The offline cache (`src-tauri/.sqlx/`) must be regenerated whenever
//! the schema changes. Mirrors the rationale in
//! `swarm/coordinator/store/`: forcing a multi-step ritual onto a
//! straightforward append-only log was strictly worse than runtime-
//! checked `sqlx::query`.
//!
//! ## Persistence shape
//!
//! Three roles share one TEXT `content` column. The role tag selects
//! the parser:
//!
//! - `User` — `content` is the raw user text.
//! - `Orchestrator` — `content` is a JSON-encoded
//!   `OrchestratorOutcome` (action + text + reasoning packed for
//!   round-trip).
//! - `Job` — `content` is the dispatched `job_id`; the refined goal
//!   travels in the dedicated `goal` column.
//!
//! Tradeoff documented in the migration file: schema simplicity
//! beats column-per-shape proliferation when the only access pattern
//! is "list recent N for workspace X".
//!
//! Cross-runtime hygiene: this module imports only from `serde`,
//! `sqlx::Row`, `specta`, and the orchestrator types in the same
//! coordinator subtree. No `sidecar/`, no `agent_runtime/`, no Tauri
//! runtime — the helpers are pure storage operations the IPC handler
//! drives.
//!
//! ## Module layout
//!
//! Split from a single 675-line file into responsibility-keyed
//! submodules; the public surface
//! (`crate::swarm::coordinator::orchestrator_session::{…}`) is
//! preserved verbatim via the re-exports below.
//!
//! - [`model`] — chat-message wire types ([`OrchestratorMessageRole`]
//!   + [`OrchestratorMessage`]).
//! - [`store`] — SQLite append / list / clear helpers.
//! - [`render`] — [`render_with_history`] prompt assembly.

mod model;
mod render;
mod store;

#[cfg(test)]
mod tests;

pub use model::{OrchestratorMessage, OrchestratorMessageRole};
pub(crate) use render::render_with_history;
pub(crate) use store::{
    append_job_message, append_orchestrator_message, append_user_message,
    clear_messages, list_recent_messages,
};

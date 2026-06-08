//! `MailboxBus` — typed event-bus over the W2-02 `mailbox` table
//! (WP-W5-01).
//!
//! Two responsibilities:
//!
//! 1. **Persist** structured events to the SQL log. The
//!    migration 0010 columns (`kind`, `parent_id`, `payload_json`)
//!    extend the existing append-only mailbox so a process restart
//!    can replay the full event history. The `MailboxEvent` enum
//!    captures every shape the W5 swarm needs (task dispatch,
//!    agent result, help requests, job lifecycle).
//!
//! 2. **Broadcast** events in-process so subscribers (W5-02
//!    agent dispatchers, W5-03 Coordinator brain, W5-04
//!    job-state projector) wake on emit instead of polling the
//!    SQL log every 2s. Per-workspace `tokio::sync::broadcast`
//!    channels keep workspaces isolated and cheap to create
//!    on-demand.
//!
//! ## Wire form vs SQL form
//!
//! `MailboxEvent` uses `#[serde(tag = "kind", rename_all =
//! "snake_case")]`. The serde tag value (`task_dispatch`,
//! `agent_result`, …) is the same string written to the SQL
//! `kind` column AND emitted on the JSON IPC bindings — one form,
//! no conversion table to maintain. The legacy `type` column
//! (W2-02 + W4-07: values like `task:done`, `swarm.help_request`)
//! stays untouched on legacy emit paths; W5-01's new emitters
//! keep `summary` for the human-readable line and put the
//! discriminated payload in `kind` + `payload_json`.
//!
//! ## Persistence vs broadcast atomicity
//!
//! `emit_typed` runs in this order:
//! 1. INSERT row (autoincrement id) inside one SQL statement.
//! 2. Build the `MailboxEnvelope` from the row + event.
//! 3. `broadcast::Sender::send` to in-process subscribers.
//!    Errors when no receivers are attached are silently swallowed
//!    (the SQL row IS the source of truth; broadcast is a
//!    wake-up optimization).
//! 4. `app.emit("mailbox:new", legacy_form)` so existing frontend
//!    listeners (terminal pane mailbox panel) keep working.
//!
//! SQL failure short-circuits the rest — the broadcast and Tauri
//! event are skipped so subscribers never see a broadcast that
//! never made it to disk.
//!
//! Out of scope (per WP §"Out of scope"):
//! - Agent-side subscription wiring (W5-02 owns the
//!   `MailboxAgentDispatcher` task).
//! - Coordinator brain dispatch loop (W5-03).
//! - Job-state derivation from mailbox (W5-04).
//! - Cancel propagation through the bus (W5-05).
//! - FSM teardown (W5-06).
//!
//! ## Module layout (refactor, DEEP)
//!
//! This used to be a single ~1460-line `mailbox_bus.rs`. It is now
//! a package that splits the three concerns into sibling submodules
//! and re-exports the public symbols at the same path
//! (`swarm::mailbox_bus::{MailboxBus, MailboxEnvelope, MailboxEvent}`)
//! so `swarm::mod`'s `pub use mailbox_bus::{…}` and every
//! `crate::swarm::mailbox_bus::*` consumer (projector, brain,
//! agent_dispatcher, commands) keep resolving without change:
//!
//! - [`event`] — the `MailboxEvent` tagged enum (pure type, SQL
//!   `kind`/`payload_json` round-trip helpers).
//! - [`envelope`] — `MailboxEnvelope`, the persisted-row + event
//!   wire shape returned by `emit_typed` / `list_typed`.
//! - [`bus`] — the stateful `MailboxBus` (per-workspace broadcast
//!   channels + persist/broadcast/Tauri-emit surface +
//!   workspace-busy guard + shutdown cancel fan-out).

mod bus;
mod envelope;
mod event;

#[cfg(test)]
mod tests;

pub use bus::MailboxBus;
pub use envelope::MailboxEnvelope;
pub use event::MailboxEvent;

//! Unit tests for the `mailbox_bus` package. Split from the former
//! single-file `tests.rs` (832L) into per-concern submodules (DEEP)
//! that mirror the production split (`event` / `envelope` / `bus`).
//! The test bodies are moved verbatim — only the module wrapping
//! changed; every test reaches the package types through the
//! `crate::swarm::mailbox_bus` re-exports, so the assertions are the
//! byte-for-byte pre-split versions.
//!
//! The pre-split file claimed the tests shared `sample_events` /
//! `seed_swarm_job` across concerns; they do not. `sample_events`
//! belongs to the [`event`] round-trip suite and `seed_swarm_job` to
//! the [`guard`] suite, so each helper now lives with its only
//! consumer (no shared scaffolding in `mod.rs`).
//!
//! Layout:
//! - [`migration`] — migration 0010 column round-trip.
//! - [`event`] — `MailboxEvent` serde / `from_row_parts` round-trip.
//! - [`bus`] — `MailboxBus` subscribe / `emit_typed` / `list_typed`.
//! - [`guard`] — WP-W5-05 workspace-busy guard + shutdown cancel
//!   fan-out (both share the local `seed_swarm_job` fixture).

mod bus;
mod event;
mod guard;
mod migration;

//! Tauri command modules.
//!
//! Layout follows AGENTS.md §"Path conventions": one file per
//! command namespace.
//!
//! WP-W2-02 shipped a single `health` namespace as a smoke surface
//! for the DB pool wiring. WP-W2-03 layers in the six domain
//! namespaces enumerated in `docs/work-packages/WP-W2-03-command-surface.md`:
//! `agents`, `workflows`, `runs`, `mcp`, `terminal`, `mailbox`.
//! All exposed commands are aggregated in `lib.rs` via
//! `tauri_specta::collect_commands![]`.

pub mod agents;
pub mod health;
pub mod mailbox;
pub mod mcp;
pub mod me;
pub mod runs;
pub mod secrets;
pub mod settings;
pub mod swarm;
pub mod swarm_term;
pub mod terminal;
pub mod util;
pub mod workflows;

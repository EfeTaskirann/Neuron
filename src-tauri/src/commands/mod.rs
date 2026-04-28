//! Tauri command modules.
//!
//! Layout follows AGENTS.md §"Path conventions": one file per
//! command namespace. WP-W2-02 ships a single `health` namespace
//! purely as a smoke-test surface for the DB pool wiring; real
//! domain commands (`agents:list`, `runs:list`, …) arrive in
//! WP-W2-03.

pub mod health;

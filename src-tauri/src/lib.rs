// WP-W2-01 set up the Tauri 2 entry point.
// WP-W2-02 layered in sqlx, the migrator, and a single `health_db`
// smoke command.
// WP-W2-03 introduced the full domain command surface, wired through
// `tauri-specta` so the typed frontend bindings live at
// `app/src/lib/bindings.ts`.
//
// The crate root is intentionally thin: module declarations plus the
// two crate-level entry points re-exported from `bootstrap/` —
// `run()` (consumed by `main.rs`) and `specta_builder_for_export()`
// (consumed by `src/bin/export-bindings.rs`).
//
// Binding generation strategy
// ---------------------------
// The TS bindings are produced by a one-shot binary
// (`src/bin/export-bindings.rs`) that re-uses
// `specta_builder_for_export()`:
//
// ```sh
// cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings
// ```
//
// We chose the binary over a `build.rs` to keep the Rust/TS pipelines
// independent — `pnpm typecheck` does not need to invoke `cargo
// build`, and CI can regenerate by running the bin once. See
// WP-W2-03 § "Notes / risks" for the choice rationale.

mod bootstrap;

pub mod commands;
pub mod db;
pub mod error;
pub mod events;
pub mod mcp;
pub mod models;
pub mod secrets;
pub mod sidecar;
pub mod swarm;
pub mod swarm_term;
pub mod telemetry;
pub mod text;
pub mod time;
pub mod tuning;

#[cfg(test)]
pub mod test_support;

pub use bootstrap::{run, specta_builder_for_export};

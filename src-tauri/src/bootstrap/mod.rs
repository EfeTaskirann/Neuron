//! App bootstrap — the Tauri builder assembly, split by lifecycle
//! phase (extracted from the former ~500-line `lib.rs`):
//!
//! - [`specta`] — the tauri-specta builder: every IPC command +
//!   explicitly-registered event payload type (the single source the
//!   `export-bindings` binary reads).
//! - [`setup`] — the `setup` hook body: logger, DB pool, sidecar,
//!   swarm registries, mailbox bus, projectors, terminal registries,
//!   OTLP export loop (dependency-ordered).
//! - [`exit`] — the `RunEvent::ExitRequested` teardown sequence
//!   (supervisor shutdowns in dependency order).
//!
//! The public surface stays at the crate root: `lib.rs` re-exports
//! [`run`] and [`specta_builder_for_export`], so `main.rs` and the
//! `export-bindings` bin are unchanged.

mod exit;
mod setup;
mod specta;

pub use specta::specta_builder_for_export;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder_for_export();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| setup::setup(app, &builder))
        .build(tauri::generate_context!())
        .expect("error while building Neuron Tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                exit::handle_exit_requested(app);
            }
        });
}

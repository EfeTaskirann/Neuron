// WP-W2-01 set up the Tauri 2 entry point.
// WP-W2-02 layered in sqlx, the migrator, and a single `health_db`
// smoke command.
// WP-W2-03 introduces the full domain command surface: 17 commands
// across six namespaces (`agents`, `workflows`, `runs`, `mcp`,
// `terminal`, `mailbox`), wired through `tauri-specta` so the typed
// frontend bindings live at `app/src/lib/bindings.ts`.
//
// Binding generation strategy
// ---------------------------
// The TS bindings are produced by a one-shot binary
// (`src/bin/export-bindings.rs`) that re-uses
// `specta_builder_for_export()` below:
//
// ```sh
// cargo run --manifest-path src-tauri/Cargo.toml --bin export-bindings
// ```
//
// We chose the binary over a `build.rs` to keep the Rust/TS pipelines
// independent — `pnpm typecheck` does not need to invoke `cargo
// build`, and CI can regenerate by running the bin once. See
// WP-W2-03 § "Notes / risks" for the choice rationale.

use tauri::Manager;

pub mod commands;
pub mod db;
pub mod error;
pub mod models;

#[cfg(test)]
pub mod test_support;

/// Build the tauri-specta builder with every WP-W2-03 command and the
/// existing `health_db` smoke command registered.
///
/// Public so the `export-bindings` binary can call into it without
/// touching the runtime startup path. Tests do not need this — they
/// invoke command functions directly.
pub fn specta_builder_for_export() -> tauri_specta::Builder<tauri::Wry> {
    use tauri_specta::collect_commands;

    // Commands that touch the IPC `AppHandle` are generic over
    // `R: tauri::Runtime` so unit tests can drive them under
    // `tauri::test::MockRuntime`. The `collect_commands!` macro
    // requires explicit `::<tauri::Wry>` annotations on those
    // entries (per its own docs at
    // tauri-specta-2.0.0-rc.24/src/macros.rs:18-37).
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::health::health_db,
            commands::agents::agents_list,
            commands::agents::agents_get,
            commands::agents::agents_create::<tauri::Wry>,
            commands::agents::agents_update::<tauri::Wry>,
            commands::agents::agents_delete::<tauri::Wry>,
            commands::workflows::workflows_list,
            commands::workflows::workflows_get,
            commands::runs::runs_list,
            commands::runs::runs_get,
            commands::runs::runs_create,
            commands::runs::runs_cancel,
            commands::mcp::mcp_list,
            commands::mcp::mcp_install::<tauri::Wry>,
            commands::mcp::mcp_uninstall::<tauri::Wry>,
            commands::terminal::terminal_list,
            commands::terminal::terminal_spawn,
            commands::terminal::terminal_kill,
            commands::mailbox::mailbox_list,
            commands::mailbox::mailbox_emit::<tauri::Wry>,
        ])
        // Register the AppError once on the builder so the type lands
        // in `bindings.ts` as a referenceable shape rather than being
        // inlined into every command's Result.
        .typ::<crate::error::AppError>()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder_for_export();

    tauri::Builder::default()
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            // tauri-specta needs `mount_events` if you collect events;
            // we don't (we use raw `app.emit("mailbox.new", ...)` so
            // the WP-W2-03 spec stays tooling-agnostic), but calling
            // it is harmless and forward-compatible.
            builder.mount_events(app);

            // `db::init` is async because sqlx is. The Tauri setup
            // hook is sync, so we drive the future via Tauri's own
            // tokio runtime — the same one that hosts every command.
            let handle = app.handle().clone();
            let pool = tauri::async_runtime::block_on(async move {
                db::init(&handle).await
            })?;
            app.manage(pool);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Neuron Tauri application");
}

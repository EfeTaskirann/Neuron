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
pub mod events;
pub mod mcp;
pub mod models;
pub mod sidecar;
pub mod time;
pub mod tuning;

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
    // Namespace-grouped to make per-domain additions visually
    // self-evident. Order matches the WP-W2-03 namespace listing.
    // tauri-specta's `collect_commands!` accepts only one comma-
    // separated list (chained `.commands()` calls overwrite), so the
    // grouping is by blank-line + comment, not by separate macro calls.
    tauri_specta::Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            // health (smoke)
            commands::health::health_db,
            // agents
            commands::agents::agents_list,
            commands::agents::agents_get,
            commands::agents::agents_create::<tauri::Wry>,
            commands::agents::agents_update::<tauri::Wry>,
            commands::agents::agents_delete::<tauri::Wry>,
            // workflows
            commands::workflows::workflows_list,
            commands::workflows::workflows_get,
            // runs
            commands::runs::runs_list,
            commands::runs::runs_get,
            commands::runs::runs_create::<tauri::Wry>,
            commands::runs::runs_cancel,
            // me
            commands::me::me_get,
            // mcp
            commands::mcp::mcp_list,
            commands::mcp::mcp_install::<tauri::Wry>,
            commands::mcp::mcp_uninstall::<tauri::Wry>,
            commands::mcp::mcp_list_tools,
            commands::mcp::mcp_call_tool::<tauri::Wry>,
            // terminal
            commands::terminal::terminal_list,
            commands::terminal::terminal_spawn::<tauri::Wry>,
            commands::terminal::terminal_kill,
            commands::terminal::terminal_write,
            commands::terminal::terminal_resize,
            commands::terminal::terminal_lines,
            // mailbox
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
            // Operational logger — first thing in setup so any error
            // emitted by `db::init` or sidecar spawn lands as a
            // structured `tracing::*` event. `try_init` is panic-safe:
            // tests that build multiple `tauri::test::mock_builder`
            // apps in one process tolerate the second-init no-op.
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| {
                            tracing_subscriber::EnvFilter::new("warn,neuron=info")
                        }),
                )
                .try_init();

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

            // WP-W2-04 — spawn the LangGraph Python sidecar after the
            // pool is in app state (the read loop needs it). A spawn
            // failure is non-fatal: the app continues, but
            // `runs:create` will return `AppError::Sidecar` until the
            // user installs Python + runs `uv sync`. The sidecar
            // README shows the steps.
            match sidecar::agent::spawn_runtime(app.handle()) {
                Ok(handle) => {
                    app.manage(handle);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "LangGraph sidecar unavailable; \
                         run `cd src-tauri/sidecar/agent_runtime && uv sync` to install"
                    );
                }
            }

            // WP-W2-06 — install an empty terminal PTY registry. Each
            // `terminal:spawn` adds a pane to it; the shutdown hook
            // below tears them all down on app exit so no shell
            // processes outlive the app on next launch.
            app.manage(sidecar::terminal::TerminalRegistry::new());

            Ok(())
        })
        // WP-W2-04 §"Acceptance criteria": "Sidecar process is killed
        // on app shutdown (no orphan `python` process on next launch)".
        // WP-W2-06 §"Acceptance criteria": "Killing app cleans all
        // child PTY processes (no orphans on next launch)".
        // `RunEvent::ExitRequested` fires before the runtime stops; we
        // drive both supervisors' shutdown paths so neither the Python
        // sidecar nor any spawned shell outlives the window. The
        // `kill_on_drop(true)` we set on the agent spawn config is a
        // defensive seatbelt; `portable_pty::ChildKiller` is the
        // explicit kill API for terminals.
        .build(tauri::generate_context!())
        .expect("error while building Neuron Tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(handle) = app.try_state::<sidecar::agent::SidecarHandle>() {
                    // `State::inner()` returns `&SidecarHandle`; we clone
                    // the cheap `Arc`-backed handle into the async block
                    // so `block_on` does not borrow across the closure.
                    let cloned = handle.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        cloned.shutdown().await;
                    });
                }
                if let Some(registry) = app.try_state::<sidecar::terminal::TerminalRegistry>() {
                    let cloned = registry.inner().clone();
                    // shutdown_all needs the DB pool to flush each
                    // pane's ring buffer to `pane_lines` synchronously
                    // before this hook returns — see report.md §K1.
                    if let Some(pool) = app.try_state::<db::DbPool>() {
                        let pool_cloned = pool.inner().clone();
                        tauri::async_runtime::block_on(async move {
                            cloned.shutdown_all(&pool_cloned).await;
                        });
                    }
                }
            }
        });
}

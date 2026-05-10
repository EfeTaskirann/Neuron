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
pub mod secrets;
pub mod sidecar;
pub mod swarm;
pub mod telemetry;
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
            commands::mailbox::mailbox_emit_typed::<tauri::Wry>,
            commands::mailbox::mailbox_list_typed,
            // secrets
            commands::secrets::secrets_set,
            commands::secrets::secrets_has,
            commands::secrets::secrets_delete,
            // settings
            commands::settings::settings_get,
            commands::settings::settings_set,
            commands::settings::settings_delete,
            commands::settings::settings_list,
            // swarm
            commands::swarm::swarm_profiles_list::<tauri::Wry>,
            commands::swarm::swarm_test_invoke::<tauri::Wry>,
            commands::swarm::swarm_run_job::<tauri::Wry>,
            commands::swarm::swarm_orchestrator_decide::<tauri::Wry>,
            commands::swarm::swarm_orchestrator_history::<tauri::Wry>,
            commands::swarm::swarm_orchestrator_clear_history::<tauri::Wry>,
            commands::swarm::swarm_orchestrator_log_job::<tauri::Wry>,
            commands::swarm::swarm_cancel_job::<tauri::Wry>,
            commands::swarm::swarm_list_jobs::<tauri::Wry>,
            commands::swarm::swarm_get_job::<tauri::Wry>,
            commands::swarm::swarm_agents_list_status::<tauri::Wry>,
            commands::swarm::swarm_agents_shutdown_workspace::<tauri::Wry>,
            commands::swarm::swarm_agents_dispatch_to_agent::<tauri::Wry>,
            commands::swarm::swarm_run_job_v2::<tauri::Wry>,
        ])
        // Register the AppError once on the builder so the type lands
        // in `bindings.ts` as a referenceable shape rather than being
        // inlined into every command's Result.
        .typ::<crate::error::AppError>()
        // WP-W3-12c — `SwarmJobEvent` is the payload of the
        // `swarm:job:{id}:event` Tauri event channel. Specta only
        // walks types reachable from registered commands; events
        // are a side channel, so we register the type explicitly
        // so frontend listeners can deserialize the payload with
        // strict types instead of `unknown`.
        .typ::<crate::swarm::coordinator::SwarmJobEvent>()
        // WP-W4-03 — `SwarmAgentEvent` is the payload of the
        // per-agent event channel `swarm:agent:{ws}:{id}:event`. Same
        // explicit registration story as `SwarmJobEvent` since
        // events are a side channel; the W4-04 grid panes
        // deserialise this payload via the bindings.ts type.
        .typ::<crate::swarm::SwarmAgentEvent>()
        // WP-W4-05 — `HelpRequest` and `CoordinatorHelpOutcome` are
        // emitted on the per-agent event channel (HelpRequest variant)
        // and consumed by the FSM (W4-06) for the
        // specialist→Coordinator routing loop. Specta only walks
        // types reachable from registered commands; explicit register
        // mirrors the SwarmAgentEvent / SwarmJobEvent pattern.
        .typ::<crate::swarm::HelpRequest>()
        .typ::<crate::swarm::CoordinatorHelpOutcome>()
        // WP-W5-01 — `MailboxEvent` is the typed payload of the
        // mailbox event-bus; `MailboxEnvelope` is the wire shape
        // returned by `mailbox:emit_typed` / `mailbox:list_typed`.
        // The bus also broadcasts envelopes in-process for the
        // W5-02 agent dispatcher + W5-03 brain to subscribe to.
        // Explicit register so the tagged-enum lands in bindings.ts
        // (specta walks reachable types only).
        .typ::<crate::swarm::MailboxEvent>()
        .typ::<crate::swarm::MailboxEnvelope>()
        // WP-W5-03 — `BrainAction` is the discriminated-union the
        // Coordinator persona emits per turn (dispatch / finish /
        // ask_user / help_outcome). It's not a command surface; it
        // appears in tests and serializes to mailbox `payload_json`
        // through the W5-02 dispatcher's help-loop branch.
        // Registering it on the specta builder lands the type in
        // bindings.ts so a future frontend that wants to display
        // "Coordinator emitted: dispatch agent:scout" can match on
        // the discriminator.
        .typ::<crate::swarm::BrainAction>()
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

            // WP-W3-12a / W3-12b — install the SQLite-backed swarm
            // `JobRegistry`. `swarm:run_job` serializes per-workspace
            // calls through it; `Arc` so multiple concurrent commands
            // share the same lock state. The pool comes from the
            // already-managed `DbPool` so every state transition
            // writes through to `swarm_jobs` / `swarm_stages` /
            // `swarm_workspace_locks`.
            //
            // BEFORE `app.manage(registry)`: run the orphan recovery
            // sweep so any non-terminal job left over by a previous
            // process is finalized as `Failed { last_error: 'interrupted
            // by app restart' }` and its workspace lock cleared. The
            // recovered set is hydrated into the registry's in-memory
            // cache, capped at 100 rows.
            //
            // Placed BEFORE the TerminalRegistry per WP-W3-12b so any
            // future code path that constructs `CoordinatorFsm` during
            // startup finds the registry in app state.
            let pool_for_registry = app
                .state::<db::DbPool>()
                .inner()
                .clone();
            let job_registry = std::sync::Arc::new(
                crate::swarm::coordinator::JobRegistry::with_pool(
                    pool_for_registry,
                ),
            );
            let registry_for_recovery = std::sync::Arc::clone(&job_registry);
            let recovered = tauri::async_runtime::block_on(async move {
                registry_for_recovery.recover_orphans().await
            })?;
            if recovered > 0 {
                tracing::warn!(
                    count = recovered,
                    "swarm: recovered {} orphan job(s) interrupted by previous process",
                    recovered
                );
            }
            app.manage(job_registry);

            // WP-W4-02 — install the workspace-scoped
            // `SwarmAgentRegistry`. Owns the lifecycle of the W4-01
            // `PersistentSession`s: lazy-spawn on first acquire,
            // turn-cap respawn under `NEURON_SWARM_AGENT_TURN_CAP`,
            // eager kill on app close (handled in the
            // `RunEvent::ExitRequested` branch below).
            //
            // Bundled profiles are loaded once here and shared (Arc)
            // by the registry. Workspace-override profiles
            // (`<app_data_dir>/agents/*.md`) re-resolve per IPC call
            // via `swarm_profiles_list` / `swarm_run_job`; they're
            // not cached on the registry, so a user editing a
            // workspace override mid-session will see it on the
            // *next* registry method call (consistent with the rest
            // of the swarm namespace).
            let workspace_agents_dir =
                app.path().app_data_dir().ok().map(|p| p.join("agents"));
            let workspace_agents_dir =
                workspace_agents_dir.filter(|p| p.is_dir());
            let bundled_profiles = std::sync::Arc::new(
                crate::swarm::ProfileRegistry::load_from(
                    workspace_agents_dir.as_deref(),
                )?,
            );
            let agent_registry = std::sync::Arc::new(
                crate::swarm::SwarmAgentRegistry::new(bundled_profiles),
            );
            app.manage(agent_registry);

            // WP-W5-01 — install the mailbox event-bus. Per-workspace
            // `tokio::broadcast` channels lazy-create on first
            // subscribe / emit; the bus shares the same `DbPool` as
            // the rest of the app so emits land in the existing
            // `mailbox` table (extended with kind / parent_id /
            // payload_json columns by migration 0010).
            //
            // Dependency order is: pool → JobRegistry → AgentRegistry
            // → MailboxBus. The bus needs the pool but does not
            // depend on either registry; it lands here so future
            // setup code (W5-02 dispatcher, W5-03 brain) can read
            // both `Arc<SwarmAgentRegistry>` and `Arc<MailboxBus>`
            // from app state without ordering surprises.
            let pool_for_bus = app
                .state::<db::DbPool>()
                .inner()
                .clone();
            let mailbox_bus = std::sync::Arc::new(
                crate::swarm::MailboxBus::new(pool_for_bus),
            );
            app.manage(mailbox_bus);

            // WP-W5-04 — install the per-workspace `JobProjector`
            // registry. Lazy-spawned: the registry holds zero
            // projectors at startup; `swarm:run_job_v2` calls
            // `ensure_for_workspace` on its workspace before the
            // brain emits the first JobStarted, so the projector
            // is subscribed to the bus before any brain-driven
            // event lands. Per WP-W5-04 §"Notes / risks" the
            // bus's broadcast channel is FIFO per subscriber, so
            // this subscribe-before-emit ordering keeps the event
            // stream ordered from the projector's perspective.
            let projector_registry = std::sync::Arc::new(
                crate::swarm::JobProjectorRegistry::new(),
            );
            app.manage(projector_registry);

            // WP-W2-06 — install an empty terminal PTY registry. Each
            // `terminal:spawn` adds a pane to it; the shutdown hook
            // below tears them all down on app exit so no shell
            // processes outlive the app on next launch.
            app.manage(sidecar::terminal::TerminalRegistry::new());

            // WP-W3-06 — start the OTLP export sweep iff the
            // collector endpoint is configured. The loop is silent
            // (no panic / warn) when the env var is unset; users who
            // don't wire a collector get the same behaviour as
            // before this WP landed.
            //
            // Security review M3: the endpoint is validated up-front
            // (scheme allow-list + plain-HTTP-to-non-loopback warning)
            // before the loop is spawned. An invalid value skips the
            // loop entirely so a typo'd `NEURON_OTEL_ENDPOINT` cannot
            // silently become a never-exporting fire-and-forget — the
            // warning lands in the structured logs at startup.
            if let Ok(endpoint) = std::env::var("NEURON_OTEL_ENDPOINT") {
                let endpoint = endpoint.trim().to_string();
                if !endpoint.is_empty() {
                    match crate::telemetry::exporter::validate_endpoint(&endpoint) {
                        Ok(()) => {
                            let pool_for_export = app
                                .state::<db::DbPool>()
                                .inner()
                                .clone();
                            tauri::async_runtime::spawn(async move {
                                crate::telemetry::start_export_loop(
                                    pool_for_export,
                                    endpoint,
                                )
                                .await;
                            });
                        }
                        Err(reason) => {
                            tracing::warn!(
                                endpoint = %endpoint,
                                reason = %reason,
                                "NEURON_OTEL_ENDPOINT rejected; export loop not started"
                            );
                        }
                    }
                }
            }

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
                // WP-W4-02 — kill every persistent agent session
                // before the runtime exits. Same kill_on_drop seatbelt
                // as the rest of the supervisors, but explicit
                // shutdown is cleaner so claude exits via stdin EOF
                // (graceful) rather than SIGKILL on the way out.
                if let Some(agent_registry) = app
                    .try_state::<std::sync::Arc<crate::swarm::SwarmAgentRegistry>>()
                {
                    let cloned = agent_registry.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        let _ = cloned.shutdown_all().await;
                    });
                }
                // WP-W5-04 — drain every projector before exit so
                // the broadcast subscribers don't outlive the
                // runtime. Idempotent — empty registry is a no-op.
                if let Some(projector_registry) = app
                    .try_state::<std::sync::Arc<crate::swarm::JobProjectorRegistry>>()
                {
                    let cloned = projector_registry.inner().clone();
                    tauri::async_runtime::block_on(async move {
                        cloned.shutdown_all().await;
                    });
                }
            }
        });
}

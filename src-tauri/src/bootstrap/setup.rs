//! The Tauri `setup` hook body — logger, DB pool, sidecar, swarm
//! registries, mailbox bus, projectors, terminal registries, OTLP
//! export loop. Dependency order matters and is documented inline.

use tauri::Manager;

use crate::db;
use crate::sidecar;
use crate::swarm_term;

/// Everything `lib.rs::run().setup(...)` does, in dependency order.
/// Split out of the closure so the bootstrap sequence is a readable,
/// reviewable function instead of a 200-line lambda.
pub(super) fn setup(
    app: &mut tauri::App,
    builder: &tauri_specta::Builder<tauri::Wry>,
) -> Result<(), Box<dyn std::error::Error>> {
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

    // WP-W3-12a / W3-12b / W5-06 — install the SQLite-backed
    // swarm `JobRegistry`. `swarm:run_job` (now brain-driven
    // post-W5-06) serializes per-workspace calls through
    // `try_acquire_workspace`. The pool comes from the
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
    // Placed BEFORE the TerminalRegistry per WP-W3-12b. The
    // FSM was deleted in W5-06, but the registry stays
    // because the brain-driven `swarm:run_job` still relies
    // on `try_acquire_workspace` + the projector reads
    // through it for `swarm:list_jobs` / `swarm:get_job`.
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
    // `RunEvent::ExitRequested` branch in `bootstrap::exit`).
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
    // `terminal:spawn` adds a pane to it; the exit hook
    // (`bootstrap::exit`) tears them all down on app exit so no
    // shell processes outlive the app on next launch.
    app.manage(sidecar::terminal::TerminalRegistry::new());

    // Terminal-Swarm — registry holds the currently active
    // 9-pane session (one at a time). Empty at boot; the
    // `swarm_term:start_session` IPC populates it (Phase 2+).
    app.manage(std::sync::Arc::new(
        swarm_term::TerminalSwarmRegistry::new(),
    ));

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
}

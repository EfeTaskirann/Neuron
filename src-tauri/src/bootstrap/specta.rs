//! tauri-specta builder — the single registry of every IPC command
//! and every explicitly-exported event/payload type.

/// Build the tauri-specta builder with every WP-W2-03 command and the
/// existing `health_db` smoke command registered.
///
/// Public (re-exported at the crate root) so the `export-bindings`
/// binary can call into it without touching the runtime startup path.
/// Tests do not need this — they invoke command functions directly.
pub fn specta_builder_for_export() -> tauri_specta::Builder<tauri::Wry> {
    use tauri_specta::collect_commands;

    use crate::commands;

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
            commands::terminal::terminal_purge_closed,
            commands::terminal::terminal_delete,
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
            // swarm — paths resolve to the per-area submodules
            // because `#[tauri::command]` / `#[specta::specta]`
            // generate `__cmd__*` / `__specta__fn__*` helpers in
            // the module the command is defined in; the parent
            // `commands::swarm` only re-exports the user-facing
            // function symbol, not those macro helpers.
            commands::swarm::profiles::swarm_profiles_list::<tauri::Wry>,
            commands::swarm::profiles::swarm_test_invoke::<tauri::Wry>,
            commands::swarm::run::swarm_run_job::<tauri::Wry>,
            commands::swarm::orchestrator::swarm_orchestrator_decide::<tauri::Wry>,
            commands::swarm::orchestrator::swarm_orchestrator_history::<tauri::Wry>,
            commands::swarm::orchestrator::swarm_orchestrator_clear_history::<tauri::Wry>,
            commands::swarm::orchestrator::swarm_orchestrator_log_job::<tauri::Wry>,
            commands::swarm::jobs::swarm_cancel_job::<tauri::Wry>,
            commands::swarm::jobs::swarm_list_jobs::<tauri::Wry>,
            commands::swarm::jobs::swarm_get_job::<tauri::Wry>,
            commands::swarm::agents::swarm_agents_list_status::<tauri::Wry>,
            commands::swarm::agents::swarm_agents_shutdown_workspace::<tauri::Wry>,
            commands::swarm::dispatch::swarm_agents_dispatch_to_agent::<tauri::Wry>,
            // swarm-term (Terminal-Hierarchy Swarm)
            commands::swarm_term::swarm_term_list_personas::<tauri::Wry>,
            commands::swarm_term::swarm_term_session_status::<tauri::Wry>,
            commands::swarm_term::swarm_term_start_session::<tauri::Wry>,
            commands::swarm_term::swarm_term_stop_session::<tauri::Wry>,
            commands::swarm_term::swarm_term_run_update::<tauri::Wry>,
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
        // and consumed for the specialist→Coordinator routing loop.
        // Specta only walks types reachable from registered commands;
        // explicit register mirrors the SwarmAgentEvent /
        // SwarmJobEvent pattern.
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

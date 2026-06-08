//! `swarm:*` namespace — Tauri command surface for the swarm
//! substrate. WP-W3-11 introduced the first two commands
//! (`profiles_list`, `test_invoke`); W3-12 / W4 / W5 layered on
//! orchestrator + job + agent surfaces; W5-06 retired the FSM in
//! favour of the Coordinator brain (`swarm:run_job`).
//!
//! **2026-05-31 refactor (T3-01):** this file used to host every
//! command, helper, and test in a single 3043-line module. It now
//! delegates to per-area submodules (`profiles`, `orchestrator`,
//! `jobs`, `agents`, `dispatch`, `run`) and re-exports the public
//! command symbols at the same path so external paths
//! (`commands::swarm::swarm_profiles_list`, the `lib.rs`
//! `collect_commands!` list, doc-comments in
//! `crate::swarm::agent_dispatcher`/`brain`/`coordinator`) keep
//! resolving without change.
//!
//! Shared helpers that more than one submodule needs live here —
//! currently only [`workspace_agents_dir`], which `profiles` and
//! `orchestrator` both consume to locate `<app_data_dir>/agents`.
//! The `#[cfg(test)] mod tests` block (in `tests.rs`) exercises
//! every command through the re-exported symbols.

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;

// `pub mod` (not `mod`) because `lib.rs::collect_commands!` references
// the macro-generated `__cmd__*` / `__specta__fn__*` helpers via
// `commands::swarm::<area>::<command>` paths — those helpers are
// emitted in the same module as the command itself, so the submodules
// must be reachable from outside this file.
pub mod agents;
pub mod dispatch;
pub mod jobs;
pub mod orchestrator;
pub mod profiles;
pub mod run;

pub use agents::{swarm_agents_list_status, swarm_agents_shutdown_workspace};
pub use dispatch::swarm_agents_dispatch_to_agent;
pub use jobs::{swarm_cancel_job, swarm_get_job, swarm_list_jobs};
pub use orchestrator::{
    swarm_orchestrator_clear_history, swarm_orchestrator_decide,
    swarm_orchestrator_history, swarm_orchestrator_log_job,
};
pub use profiles::{swarm_profiles_list, swarm_test_invoke};
pub use run::swarm_run_job;

#[cfg(test)]
pub(crate) use run::swarm_run_job_with_invoker;

/// Resolve `<app_data_dir>/agents`. Returns `None` (no error) when
/// the directory does not exist — workspace overrides are optional
/// per WP §2. Errors reaching `app_data_dir` itself are real (the
/// platform Tauri helper failed) and surface as `Internal`.
pub(super) fn workspace_agents_dir<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<std::path::PathBuf>, AppError> {
    let base = app.path().app_data_dir().map_err(|e| {
        AppError::Internal(format!("app_data_dir resolution: {e}"))
    })?;
    let dir = base.join("agents");
    if dir.is_dir() {
        Ok(Some(dir))
    } else {
        Ok(None)
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //
//
// The command + acceptance tests live in `tests.rs` as one `mod tests`
// block: they share scaffolding helpers (`mock_app_with_w5_state`,
// `mock_app_with_brain_state`, `BrainScriptedInvoker`, `seed_swarm_job_row`)
// so fanning them out per submodule would duplicate that wiring. Every
// command is reachable via the `pub use` re-exports above, so the tests'
// `use super::*;` resolves the same as the pre-split single-file version.

#[cfg(test)]
mod tests;

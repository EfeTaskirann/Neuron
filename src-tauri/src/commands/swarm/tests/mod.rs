//! Acceptance + input-validation coverage for the `swarm:*` command
//! surface, split per command area to mirror the production
//! submodules (`agents` / `dispatch` / `jobs` / `orchestrator` /
//! `profiles` / `run`). Extracted from the single-file `tests.rs`
//! (T3-24, 2026-06-14); every test reaches its command through the
//! `crate::commands::swarm` re-exports, so the assertions are the
//! byte-for-byte pre-split versions — only the module wrapping
//! changed.
//!
//! Each area's local scaffolding lives with its tests; despite the
//! pre-split file's note none of it is actually shared across areas:
//! `seed_swarm_job_row` (jobs), `mock_app_with_w5_state` (dispatch),
//! `mock_app_with_brain_state` + `BrainScriptedInvoker` (run). The one
//! cross-area helper, `mock_app_with_pool`, comes from
//! `crate::test_support`.

mod agents;
mod dispatch;
mod gate;
mod jobs;
mod orchestrator;
mod profiles;
mod run;

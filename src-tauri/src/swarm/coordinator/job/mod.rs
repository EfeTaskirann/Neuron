//! Job state types + registry (WP-W3-12a §2 / §4 + WP-W3-12b §2/§3).
//!
//! `Job`, `JobState`, `JobOutcome`, and `StageResult` cross the IPC
//! boundary as the FSM's contract with the frontend. `JobRegistry` is
//! the in-memory store with optional SQLite write-through (W3-12b).
//!
//! The registry also owns the **per-workspace lock** map. Per the
//! owner directive 2026-05-05 ("Aynı proje için yeni bir 9 kişilik
//! ekibi çalıştırmama izin vermesin, başka bir proje için izin
//! versin."), `swarm:run_job` calls with the same `workspace_id`
//! serialize (second one rejected with `AppError::WorkspaceBusy`),
//! while different `workspace_id`s run independently in parallel.
//!
//! **Refactor (T3-02, DEEP):** this used to be a single ~1470-line
//! `job.rs`. It is now a package that splits the four concerns into
//! sibling submodules and re-exports the public symbols at the same
//! path (`coordinator::job::{Job, JobState, JobRegistry, …}`) so
//! `coordinator::mod`'s `pub use job::{…}`, `store.rs`'s
//! `use super::job::{…}`, and every `crate::swarm::coordinator::*`
//! consumer keep resolving without change:
//!
//! - [`state`] — the `JobState` lifecycle enum (pure type, DB-string
//!   conversions).
//! - [`model`] — the data-model wire types: `StageResult`, `Job`,
//!   `JobOutcome`, `JobSummary`, `JobDetail`.
//! - [`registry`] — the stateful `JobRegistry` (in-memory store +
//!   per-workspace lock map + cancel-notify surface + optional
//!   SQLite write-through).
//! - [`event`] — `SwarmJobEvent`, the per-job lifecycle event
//!   streamed to `swarm:job:{job_id}:event`.

mod event;
mod model;
mod registry;
mod state;

#[cfg(test)]
mod tests;

pub use event::SwarmJobEvent;
pub use model::{Job, JobDetail, JobOutcome, JobSummary, StageResult};
pub use registry::JobRegistry;
pub use state::JobState;

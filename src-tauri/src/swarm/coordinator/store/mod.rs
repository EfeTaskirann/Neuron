//! SQLite write-through helpers for the swarm Coordinator
//! (WP-W3-12b §5).
//!
//! `pub(super)` only — these helpers are FSM-internal. The Tauri
//! commands call through `JobRegistry` (or, for read-only history,
//! through the `list_jobs` / `get_job_detail` helpers below by way
//! of `commands::swarm`). Direct call sites outside `coordinator/`
//! would split the persistence story.
//!
//! Why string queries (`sqlx::query`) instead of macro queries
//! (`sqlx::query!`)? The offline cache lives in
//! `src-tauri/.sqlx/` and must be regenerated whenever the schema
//! changes. Forcing CI to refresh the cache for every `swarm_*`
//! query would couple this WP to a multi-step ritual that's easy
//! to skip; the existing tree mixes both styles, so we lean on
//! the runtime-checked variant here. The compile-time cache
//! coverage that already exists (one `agents` count) is left
//! intact.
//!
//! Goal-truncation policy. `JobSummary.goal` is char-bounded to
//! 200 chars (NOT byte-bounded — Turkish characters!) at this
//! layer so the IPC always returns the right shape without runtime
//! panics on multi-byte boundaries. Truncation lives here (not at
//! the wire serialization layer) so future read paths get the
//! same shape "for free".
//!
//! ## Module layout
//!
//! - [`write`] — INSERT/UPDATE of `swarm_jobs` / `swarm_stages` and
//!   the workspace-lock bookkeeping.
//! - [`read`] — the recent-jobs list, single-job detail, raw stage
//!   rows, and full-`Job` hydration.
//! - [`recovery`] — process-start orphan sweep + its result shape.
//! - [`cols`] — JSON column codecs and goal truncation shared by
//!   the read/write sides.

mod cols;
mod read;
mod recovery;
mod write;

#[cfg(test)]
mod tests;

pub(super) use read::{get_job_detail, list_jobs, list_recent_jobs_full};
pub(super) use recovery::recover_orphans;
pub(super) use write::{
    delete_workspace_lock, insert_job_and_lock, insert_stage, update_job,
};

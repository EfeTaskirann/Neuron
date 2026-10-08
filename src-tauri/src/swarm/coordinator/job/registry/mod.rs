//! `JobRegistry` — in-memory job + workspace-lock store with optional
//! SQLite write-through (WP-W3-12a §4 / WP-W3-12b §2/§3).
//!
//! The registry owns the **per-workspace lock** map. Per the owner
//! directive 2026-05-05 ("Aynı proje için yeni bir 9 kişilik ekibi
//! çalıştırmama izin vermesin, başka bir proje için izin versin."),
//! `swarm:run_job` calls with the same `workspace_id` serialize
//! (second one rejected with `AppError::WorkspaceBusy`), while
//! different `workspace_id`s run independently in parallel.
//!
//! Lock acquisition order is `workspace_locks` → `jobs`, consistent
//! across every method. Two independent mutexes keep job mutations
//! that don't touch lock state cheap, but every acquire-or-release
//! traverses both in the same order so two threads can never spin in
//! a deadlock.
//!
//! W3-12b adds an optional `Option<DbPool>` so the registry doubles
//! as a write-through to `swarm_jobs` / `swarm_stages` /
//! `swarm_workspace_locks`. Mutators (`try_acquire_workspace`,
//! `update`, `release_workspace`) become `async fn` and await SQL
//! inline. SQL failure rolls back the in-memory mutation and surfaces
//! `AppError::Internal`.
//!
//! **Refactor (T3-26, DEEP):** this used to be a single ~500-line
//! `registry.rs`. It is now a package that splits the registry's
//! concerns across sibling submodules — all `impl JobRegistry`
//! blocks, so every `pub` method keeps resolving on the same type
//! and the public surface is unchanged:
//!
//! - this `mod.rs` — the `JobRegistry` struct definition, the
//!   constructors (`new` / `with_pool`), the read-only in-memory
//!   accessors (`get` / `list` / `pool` / `has_pool`), and `Default`.
//! - [`mutate`] — the write-through mutators + workspace-lock
//!   lifecycle: `try_acquire_workspace`, `release_workspace`,
//!   `update` (+ `rollback_update`), `recover_orphans`.
//! - [`cancel`] — the WP-W3-12c cancel-notify surface:
//!   `register_cancel`, `unregister_cancel`, `signal_cancel`.
//! - [`query`] — the persisted-history read accessors used by the
//!   IPC layer: `list_jobs`, `get_job_detail`.
//!
//! The struct fields stay private; the submodules reach them as
//! descendants of this module.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::db::DbPool;

use super::model::Job;

mod cancel;
mod mutate;
mod query;

/// In-memory job + workspace-lock registry, optionally backed by
/// SQLite write-through (W3-12b).
///
/// `Mutex` (std, not async) is fine here — every operation is
/// short and never `await`s while holding the lock. The async
/// mutators acquire the in-memory lock, mutate, drop it, *then*
/// await SQL.
///
/// The two maps are independent so a job mutation that doesn't
/// change workspace state (e.g. appending a `StageResult`) never
/// contests the `workspace_locks` mutex.
///
/// Lock acquisition order — both internal helpers and external
/// callers MUST follow `workspace_locks` first, then `jobs`. The
/// constructor / accessors that touch only one map are exempt.
/// **W3-12b extension**: with write-through, the order is now
/// `workspace_locks (in-mem) → jobs (in-mem) → DB tx`. All
/// in-memory locks are released BEFORE awaiting on the DB.
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    /// `workspace_id` → `job_id` of the in-flight job currently
    /// holding the workspace. Removed on `release_workspace`.
    workspace_locks: Mutex<HashMap<String, String>>,
    /// Per-job cancellation notify (W3-12c). Process-local; never
    /// persisted to SQLite.
    cancel_notifies: Mutex<HashMap<String, Arc<Notify>>>,
    /// Optional pool — `None` when the registry is in-memory only
    /// (test bench), `Some` in production. The mutators branch on
    /// `pool.as_ref()` so the same surface drives both modes.
    pool: Option<DbPool>,
}

impl JobRegistry {
    /// Build an in-memory-only registry. Used by tests and as the
    /// fallback for any call site that does not yet have access to
    /// the pool. Production code paths must use [`Self::with_pool`].
    pub fn new() -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            workspace_locks: Mutex::new(HashMap::new()),
            cancel_notifies: Mutex::new(HashMap::new()),
            pool: None,
        }
    }

    /// Build a SQLite-backed registry. Every successful in-memory
    /// mutation is mirrored to `swarm_jobs` / `swarm_stages` /
    /// `swarm_workspace_locks`. SQL failure rolls back the
    /// in-memory mutation and surfaces `AppError::Internal`.
    pub fn with_pool(pool: DbPool) -> Self {
        Self {
            jobs: Mutex::new(HashMap::new()),
            workspace_locks: Mutex::new(HashMap::new()),
            cancel_notifies: Mutex::new(HashMap::new()),
            pool: Some(pool),
        }
    }

    /// Test/inspection accessor — returns whether SQL write-through
    /// is enabled for this registry. Internal helpers in the FSM
    /// don't need this, but tests use it to assert that
    /// `with_pool(...)` actually wired through.
    #[cfg(test)]
    pub(crate) fn has_pool(&self) -> bool {
        self.pool.is_some()
    }

    /// Snapshot of the job under `id`, or `None`. Returns an owned
    /// `Job` so the caller doesn't hold the registry lock past the
    /// call. Reads the in-memory cache only — SQLite-backed history
    /// queries go through `commands::swarm::swarm_get_job`.
    pub fn get(&self, id: &str) -> Option<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.get(id).cloned()
    }

    /// Snapshot of every job in the in-memory cache. Order
    /// unspecified.
    pub fn list(&self) -> Vec<Job> {
        let jobs = self.jobs.lock().expect("jobs mutex poisoned");
        jobs.values().cloned().collect()
    }

    /// Accessor used by the IPC layer to read history rows directly
    /// from the DB. Returns `None` when the registry is in-memory
    /// only.
    pub fn pool(&self) -> Option<&DbPool> {
        self.pool.as_ref()
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

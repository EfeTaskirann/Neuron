//! WP-W3-12c cancel-notify surface for [`JobRegistry`].
//!
//! LOCK ORDER: `workspace_locks → cancel_notifies → jobs`. The three
//! methods below each hold *only* the `cancel_notifies` mutex while
//! running; they never acquire `workspace_locks` or `jobs` while
//! holding it, so they cannot deadlock against the
//! acquire/release/update/get methods (which hold at most one of the
//! three at a time).

use std::sync::Arc;

use tokio::sync::Notify;

use crate::error::AppError;

use super::JobRegistry;

impl JobRegistry {
    /// Register a cancellation `Notify` for the in-flight `job_id`.
    /// Process-local; never persisted (see W3-12b "cancel state is
    /// process-local").
    pub fn register_cancel(
        &self,
        job_id: &str,
        notify: Arc<Notify>,
    ) -> Result<(), AppError> {
        let mut notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        if notifies.contains_key(job_id) {
            return Err(AppError::Conflict(format!(
                "swarm job `{job_id}` already has a cancel notify registered"
            )));
        }
        notifies.insert(job_id.to_string(), notify);
        Ok(())
    }

    /// Idempotent remove of the cancel notify for `job_id`. Called
    /// on every FSM tail (success, failure, cancellation) plus by
    /// the `CancelGuard` Drop seatbelt — calling twice (or against
    /// an unknown id) is a no-op.
    pub fn unregister_cancel(&self, job_id: &str) {
        let mut notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        notifies.remove(job_id);
    }

    /// Signal cancellation for the given `job_id`.
    ///
    /// W3-12j: switched from `notify_one()` to `notify_waiters()` so
    /// every task currently `await`-ing the per-job `Notify` wakes —
    /// the Fullstack parallel dispatch races two `tokio::select!`s on
    /// the same `Notify`, and `notify_one` would only wake the first.
    /// `notify_waiters` is a no-op when there are no current waiters
    /// (no permit is stored), but every FSM stage call sequences
    /// `notified()` on the same handle before any awaitable point, so
    /// the parallel-track waiters are always registered by the time a
    /// user-driven cancel arrives.
    pub fn signal_cancel(&self, job_id: &str) -> Result<(), AppError> {
        let notifies = self
            .cancel_notifies
            .lock()
            .expect("cancel_notifies mutex poisoned");
        match notifies.get(job_id) {
            Some(notify) => {
                notify.notify_waiters();
                Ok(())
            }
            None => Err(AppError::NotFound(format!(
                "swarm job `{job_id}` has no in-flight cancel notify"
            ))),
        }
    }
}

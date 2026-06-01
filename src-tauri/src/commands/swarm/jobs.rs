//! `swarm:cancel_job` / `swarm:list_jobs` / `swarm:get_job` — read +
//! cancel surface for persisted swarm jobs.
//!
//! `swarm:cancel_job` discriminates on `swarm_jobs.source` between
//! the W5-03 brain-driven path (emit `MailboxEvent::JobCancel` on
//! the workspace's bus) and the legacy W3 FSM path (signal the
//! in-memory `JobRegistry` cancel notify); see WP-W5-05.

use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::swarm::coordinator::{JobDetail, JobState, JobSummary};
use crate::swarm::{JobRegistry, MailboxBus, MailboxEvent};

/// Default page size for `swarm:list_jobs`. WP-W3-12b §4.
const SWARM_LIST_JOBS_DEFAULT_LIMIT: u32 = 50;
/// Hard cap to prevent runaway queries (full pagination is W3-14).
const SWARM_LIST_JOBS_MAX_LIMIT: u32 = 200;

/// Signal cancellation for an in-flight swarm job (WP-W3-12c §4,
/// WP-W5-05 source-switching).
///
/// Discriminates on `swarm_jobs.source` (`'brain'` vs `'fsm'`):
///
/// - **`source='brain'`** (W5-03 brain-driven jobs): emits
///   `MailboxEvent::JobCancel` on the workspace's mailbox bus.
///   The brain's dispatch-loop `select!` and every dispatcher's
///   in-flight invoke notify pick it up and unwind. Returns
///   `Ok(())` immediately — the IPC does not block on the
///   cancel actually propagating (≤ 100ms in practice; same
///   async semantics as the FSM cancel below).
/// - **`source='fsm'` or no DB row** (legacy / W3 path): keeps
///   the in-memory `JobRegistry::signal_cancel` flow. Returns:
///     - `Ok(())` if the cancel signal landed on the FSM's per-
///       job `Notify`. The FSM observes the signal at its next
///       `select!` point, emits `Cancelled` then `Finished`,
///       and finalizes the job as `Failed` with `last_error =
///       "cancelled by user"`.
///     - `Err(AppError::NotFound)` if no job with the given id
///       exists in the registry.
///     - `Err(AppError::Conflict)` if the job is already
///       terminal (`Done`/`Failed`) — including a previous
///       cancel that has already finalized.
/// - **unknown source string**: returns
///   `AppError::Internal(...)`. Defensive — only `'brain'` and
///   `'fsm'` are written in production; any other value implies
///   schema drift.
///
/// Idempotency (FSM path): a second cancel against the same
/// in-flight job either returns `Ok(())` (signal sent again, FSM
/// ignores it once finalized) or `Err(Conflict)` if the FSM has
/// already removed the cancel notify on its tail. The race is
/// benign; callers should treat both as "cancel acknowledged".
///
/// Idempotency (brain path): the bus has no dedupe — a second
/// cancel emits a second `JobCancel` row. The brain + dispatchers
/// both treat the second as a no-op (the loop has already
/// terminated). The mailbox row is informational; the SQL log
/// remains the source of truth.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_cancel_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<(), AppError> {
    if job_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "jobId must not be empty".into(),
        ));
    }

    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();

    // WP-W5-05 — discriminate on `swarm_jobs.source`. Tests in the
    // FSM cancel suite use a `JobRegistry::new()` (no pool) so the
    // `swarm_jobs` row is never written; in that case the source
    // query returns `None` and we fall through to the legacy FSM
    // path. Production always has the pool wired.
    let source: Option<String> =
        if let Some(pool) = app.try_state::<crate::db::DbPool>() {
            sqlx::query_scalar(
                "SELECT source FROM swarm_jobs WHERE id = ?",
            )
            .bind(&job_id)
            .fetch_optional(pool.inner())
            .await?
        } else {
            None
        };

    match source.as_deref() {
        Some("brain") => {
            // Look up the workspace_id so the JobCancel lands on
            // the right per-workspace broadcast channel. The
            // brain + dispatcher subscribers filter by job_id,
            // but the channel routing is per-workspace.
            let pool = app
                .try_state::<crate::db::DbPool>()
                .ok_or_else(|| {
                    AppError::Internal(
                        "DbPool missing from app state — \
                         lib.rs::setup did not call app.manage()"
                            .into(),
                    )
                })?
                .inner()
                .clone();
            let workspace_id: String = sqlx::query_scalar(
                "SELECT workspace_id FROM swarm_jobs WHERE id = ?",
            )
            .bind(&job_id)
            .fetch_one(&pool)
            .await?;
            let bus = app
                .try_state::<Arc<MailboxBus>>()
                .ok_or_else(|| {
                    AppError::Internal(
                        "MailboxBus missing from app state — \
                         lib.rs::setup did not call app.manage()"
                            .into(),
                    )
                })?
                .inner()
                .clone();
            bus.emit_typed(
                &app,
                &workspace_id,
                "agent:user",
                "agent:coordinator",
                &format!("cancel requested for {job_id}"),
                None,
                MailboxEvent::JobCancel {
                    job_id: job_id.clone(),
                },
            )
            .await?;
            Ok(())
        }
        Some("fsm") | None => {
            // Legacy W3 FSM path — preserved verbatim from
            // WP-W3-12c §4. Stays in place until W5-06 deletes
            // the FSM.
            let job = registry.get(&job_id).ok_or_else(|| {
                AppError::NotFound(format!("swarm job `{job_id}`"))
            })?;
            if matches!(job.state, JobState::Done | JobState::Failed) {
                return Err(AppError::Conflict(format!(
                    "swarm job `{job_id}` is already terminal ({:?})",
                    job.state
                )));
            }
            match registry.signal_cancel(&job_id) {
                Ok(()) => Ok(()),
                Err(AppError::NotFound(_)) => {
                    Err(AppError::Conflict(format!(
                        "swarm job `{job_id}` is already terminal"
                    )))
                }
                Err(other) => Err(other),
            }
        }
        Some(other) => Err(AppError::Internal(format!(
            "unknown swarm_jobs.source: {other}"
        ))),
    }
}

/// List recent swarm jobs from persisted history (WP-W3-12b §4).
///
/// `workspace_id` filters on the indexed `swarm_jobs.workspace_id`
/// column when supplied. `limit` defaults to 50 and is hard-capped
/// at 200 — full pagination is W3-14's UI surface.
///
/// Returns an empty `Vec` (not `Err`) when the registry is in-memory
/// only (no pool wired) — that's the test harness path; production
/// always has the pool.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_list_jobs<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<JobSummary>, AppError> {
    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    let effective_limit = limit
        .unwrap_or(SWARM_LIST_JOBS_DEFAULT_LIMIT)
        .min(SWARM_LIST_JOBS_MAX_LIMIT);
    registry
        .list_jobs(workspace_id.as_deref(), effective_limit)
        .await
}

/// Fetch the full detail (job + every persisted stage) for one job
/// (WP-W3-12b §4). Unknown ids surface as `AppError::NotFound`.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_get_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<JobDetail, AppError> {
    if job_id.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "jobId must not be empty".into(),
        ));
    }
    let registry = app
        .try_state::<Arc<JobRegistry>>()
        .ok_or_else(|| {
            AppError::Internal(
                "swarm JobRegistry missing from app state — \
                 lib.rs::setup did not call app.manage()"
                    .into(),
            )
        })?
        .inner()
        .clone();
    match registry.get_job_detail(&job_id).await? {
        Some(detail) => Ok(detail),
        None => Err(AppError::NotFound(format!("swarm job {job_id}"))),
    }
}

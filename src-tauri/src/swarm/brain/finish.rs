//! Terminal `JobFinished` emitters shared by the brain's failure
//! and cancel paths.
//!
//! Split out of the monolithic `brain.rs` (WP-W5-03). Both helpers
//! emit a `JobFinished { outcome: "failed", .. }` envelope and
//! return the matching [`super::BrainRunResult`]; behaviour is
//! unchanged.

use tauri::{AppHandle, Runtime};

use crate::error::AppError;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEvent};

use super::BrainRunResult;

/// Emit `JobFinished { outcome: "failed", summary: "cancelled by user" }`
/// and return the corresponding [`BrainRunResult`]. Used by both
/// the cancel branch in the invoke and the cancel branch in the
/// wait-for-event step.
pub(super) async fn finish_with_cancel<R: Runtime>(
    app: &AppHandle<R>,
    bus: &MailboxBus,
    workspace_id: &str,
    job_id: &str,
) -> Result<BrainRunResult, AppError> {
    let summary = "cancelled by user".to_string();
    bus.emit_typed(
        app,
        workspace_id,
        "agent:coordinator",
        "agent:user",
        &format!("job finished (failed): {summary}"),
        None,
        MailboxEvent::JobFinished {
            job_id: job_id.to_string(),
            outcome: "failed".to_string(),
            summary: summary.clone(),
        },
    )
    .await?;
    Ok(BrainRunResult {
        job_id: job_id.to_string(),
        outcome: "failed".to_string(),
        summary,
    })
}

/// Emit `JobFinished { outcome: "failed", summary }` and return the
/// corresponding [`BrainRunResult`]. Used for parse failures,
/// session crashes, and the max-dispatch cap.
pub(super) async fn finish_with_failure<R: Runtime>(
    app: &AppHandle<R>,
    bus: &MailboxBus,
    workspace_id: &str,
    job_id: &str,
    summary: &str,
) -> Result<BrainRunResult, AppError> {
    bus.emit_typed(
        app,
        workspace_id,
        "agent:coordinator",
        "agent:user",
        &format!("job finished (failed): {summary}"),
        None,
        MailboxEvent::JobFinished {
            job_id: job_id.to_string(),
            outcome: "failed".to_string(),
            summary: summary.to_string(),
        },
    )
    .await?;
    Ok(BrainRunResult {
        job_id: job_id.to_string(),
        outcome: "failed".to_string(),
        summary: summary.to_string(),
    })
}

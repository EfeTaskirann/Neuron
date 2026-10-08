//! Running / awaiting-approval status transitions driven by the reader.

use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Runtime};

use crate::db::DbPool;
use crate::models::ApprovalBanner;
use crate::tuning::APPROVAL_WINDOW_LINES;

use super::super::approval::{extract_approval_blob, matches_awaiting_approval};
use super::super::{status, PaneState};

/// Inspect the last APPROVAL_WINDOW_LINES of the ring against the
/// awaiting-approval regex set for the active agent. Flip status to
/// `awaiting_approval` on a match if currently `running`. Reverse
/// transition (`awaiting_approval` → `running`) is implicit: the next
/// emitted line that does not match leaves the status at
/// `awaiting_approval` until the child either continues (real shells
/// rarely re-trigger the regex) or exits. We do not auto-clear because
/// per WP-W2-06 the explicit transition out of `awaiting_approval` is
/// the user's input, which the frontend models with its own UI.
///
/// On the AWAITING transition we also try to extract a structured
/// `ApprovalBanner` blob (`{tool, target, added, removed}`) from the
/// trailing window and stamp the JSON into `panes.last_approval_json`
/// so `terminal_list` can surface `Pane.approval` for the UI's amber
/// banner strip per `NEURON_TERMINAL_REPORT.md` § Visual contract.
/// Extraction is best-effort: when regex parsing fails (which is the
/// common case in Week 2 — agent CLIs do not yet emit a stable
/// machine-readable approval line) we persist a placeholder blob so
/// the frontend at least has a non-null banner to render.
pub(super) fn maybe_transition_status<R: Runtime>(
    state: &Arc<Mutex<PaneState>>,
    app: &AppHandle<R>,
    pool: &DbPool,
    pane_id: &str,
    agent_kind: &str,
) {
    enum Transition {
        Awaiting(ApprovalBanner),
        Running,
    }

    let transition = {
        let guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.status == status::AWAITING || guard.status == status::SUCCESS
            || guard.status == status::ERROR
        {
            return;
        }
        // Capture the trailing window without holding the lock long.
        let tail: Vec<String> = guard
            .ring
            .iter()
            .rev()
            .take(APPROVAL_WINDOW_LINES)
            .rev()
            .map(|l| l.text.clone())
            .collect();
        if tail.is_empty() {
            return;
        }
        let combined = tail.join("\n");
        if matches_awaiting_approval(agent_kind, &combined) {
            Transition::Awaiting(extract_approval_blob(agent_kind, &combined))
        } else if guard.status == status::STARTING {
            Transition::Running
        } else {
            return;
        }
    };

    match transition {
        Transition::Awaiting(banner) => set_awaiting_approval(state, pool, pane_id, &banner),
        Transition::Running => set_status(state, pool, pane_id, status::RUNNING),
    }
    let _ = app; // app is here only to keep the lifetime symmetric
                  // with future event emissions; status transitions
                  // currently surface via the next `terminal:list`.
}

/// Flip the pane to `awaiting_approval` AND persist the JSON-encoded
/// approval banner in one round-trip. Split out from [`set_status`]
/// because the awaiting transition is the only one with a side
/// payload — every other transition just rewrites `panes.status`.
fn set_awaiting_approval(
    state: &Arc<Mutex<PaneState>>,
    pool: &DbPool,
    pane_id: &str,
    banner: &ApprovalBanner,
) {
    {
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.status == status::AWAITING {
            return;
        }
        guard.status = status::AWAITING;
    }

    // `serde_json::to_string` on a flat 4-field struct cannot fail in
    // practice, but defensively we keep the pane's status flip even
    // when serialisation does fail and write a NULL blob — better an
    // unbannered awaiting pane than a panic in the read loop.
    let blob = match serde_json::to_string(banner) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(
                pane_id = %pane_id,
                error = %e,
                "approval blob serialize failed"
            );
            None
        }
    };

    let pool = pool.clone();
    let pane_id = pane_id.to_string();
    tauri::async_runtime::spawn(async move {
        let res = sqlx::query(
            "UPDATE panes SET status = 'awaiting_approval', last_approval_json = ? \
             WHERE id = ?",
        )
        .bind(blob.as_deref())
        .bind(&pane_id)
        .execute(&pool)
        .await;
        if let Err(e) = res {
            tracing::error!(
                pane_id = %pane_id,
                error = %e,
                "awaiting_approval update failed"
            );
        }
    });
}

/// Update the pane's in-memory status and the DB row. Logs but does
/// not propagate errors — the read loop must keep going on transient
/// DB hiccups.
fn set_status(
    state: &Arc<Mutex<PaneState>>,
    pool: &DbPool,
    pane_id: &str,
    new_status: &'static str,
) {
    {
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if guard.status == new_status {
            return;
        }
        guard.status = new_status;
    }

    let pool = pool.clone();
    let pane_id = pane_id.to_string();
    tauri::async_runtime::spawn(async move {
        let res = sqlx::query("UPDATE panes SET status = ? WHERE id = ?")
            .bind(new_status)
            .bind(&pane_id)
            .execute(&pool)
            .await;
        if let Err(e) = res {
            tracing::error!(
                pane_id = %pane_id,
                error = %e,
                "pane status update failed"
            );
        }
    });
}

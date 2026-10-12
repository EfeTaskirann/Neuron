//! Shared line-event payload + ring-append helper.
//!
//! Both the reader read-loop and the waiter task append lines to the
//! per-pane ring and emit `panes:{id}:line` events; this module holds
//! the wire payload and the single helper that keeps the persisted seq
//! and the emitted seq in lock-step.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::events;
use crate::tuning::{RING_BUFFER_CAP, RING_BUFFER_DROP};

use super::super::{PaneState, RingLine};

/// Wire payload of one `panes:{id}:line` Tauri event. Per ADR-0006 the
/// event uses `:` as the separator (Tauri 2.10 panics on `.`); the
/// payload itself follows the canonical NEURON_TERMINAL_REPORT line
/// shape `{ k, text, seq }` with the addition of the agent kind so the
/// frontend can branch on it without a separate query.
#[derive(Debug, Clone, Serialize)]
pub(super) struct LineEventPayload {
    pub(super) k: &'static str,
    pub(super) text: String,
    pub(super) seq: i64,
}

/// Append one line to the ring under the per-pane mutex, allocate a
/// `seq`, and emit the corresponding `panes:{id}:line` event. Splitting
/// the seq allocation from the event emit ensures the seq the frontend
/// sees matches the seq we persisted.
pub(super) fn emit_line<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<Mutex<PaneState>>,
    pane_id: &str,
    mut payload: LineEventPayload,
    text_for_db: &str,
    kind: &'static str,
) {
    let seq = {
        let mut guard = match state.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let seq = guard.next_seq;
        guard.next_seq = seq.saturating_add(1);
        guard.ring.push_back(RingLine {
            seq,
            kind,
            text: text_for_db.to_string(),
        });
        if guard.ring.len() > RING_BUFFER_CAP {
            // Drop the oldest 1,000 to amortize the cost of trimming.
            let drop = RING_BUFFER_DROP.min(guard.ring.len());
            for _ in 0..drop {
                guard.ring.pop_front();
            }
        }
        seq
    };

    payload.seq = seq;
    let event = events::pane_line(pane_id);
    if let Err(e) = app.emit(&event, &payload) {
        tracing::error!(pane_id = %pane_id, error = %e, "terminal emit error");
    }
}

/// Emit one raw-data chunk on `panes:{id}:data`. Unlike [`emit_line`]
/// this does not touch the ring / seq — it is the *interactive* stream
/// xterm.js writes verbatim (keystroke echo, cursor, ANSI colour). The
/// caller guarantees `text` ends on a UTF-8 boundary and carries no
/// DSR-CPR queries. Empty chunks are skipped so we don't spam no-op
/// events between reads that only carried a split-char tail.
pub(super) fn emit_data<R: Runtime>(app: &AppHandle<R>, pane_id: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    let event = events::pane_data(pane_id);
    if let Err(e) = app.emit(&event, text) {
        tracing::error!(pane_id = %pane_id, error = %e, "terminal data emit error");
    }
}

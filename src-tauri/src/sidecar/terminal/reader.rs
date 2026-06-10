//! Reader + waiter background tasks.
//!
//! The reader task turns raw PTY bytes into `panes:{id}:line` events
//! and ring-buffer lines, drives the awaiting-approval detection, and
//! answers DSR-CPR cursor queries. The waiter task observes child exit,
//! finalises the DB row, and flushes the ring buffer to `pane_lines`.
//!
//! Both run on `spawn_blocking` threads because the underlying
//! `Read` / `Child::try_wait` APIs are synchronous. They mutate the
//! shared [`PaneState`] under its inner `Mutex`.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use portable_pty::Child;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::ApprovalBanner;
use crate::tuning::{
    APPROVAL_WINDOW_LINES, MAX_PENDING_BYTES, READ_CHUNK_BYTES, RING_BUFFER_CAP, RING_BUFFER_DROP,
    WAIT_POLL,
};

use super::approval::{extract_approval_blob, matches_awaiting_approval};
use super::text::{
    extract_dsr_cpr_queries, strip_csi, trim_terminal_line_end, utf8_safe_prefix_len,
};
use super::{status, PaneState, RingLine, TerminalRegistry};

// --------------------------------------------------------------------- //
// Reader task — turns PTY bytes into events + ring lines               //
// --------------------------------------------------------------------- //

/// Wire payload of one `panes:{id}:line` Tauri event. Per ADR-0006 the
/// event uses `:` as the separator (Tauri 2.10 panics on `.`); the
/// payload itself follows the canonical NEURON_TERMINAL_REPORT line
/// shape `{ k, text, seq }` with the addition of the agent kind so the
/// frontend can branch on it without a separate query.
#[derive(Debug, Clone, Serialize)]
struct LineEventPayload {
    k: &'static str,
    text: String,
    seq: i64,
}

pub(super) fn run_reader<R: Runtime>(
    mut reader: Box<dyn Read + Send>,
    state: Arc<Mutex<PaneState>>,
    app: AppHandle<R>,
    pool: DbPool,
    pane_id: String,
    agent_kind: String,
) {
    let mut buf = vec![0u8; READ_CHUNK_BYTES];
    // Byte accumulator across chunks. Using `Vec<u8>` (not `String`)
    // is the K5 fix from report.md: `String::from_utf8_lossy` on each
    // chunk replaced any multi-byte char that straddled the chunk
    // boundary with U+FFFD on both sides. Holding raw bytes until we
    // see a newline `0x0A` byte is safe because UTF-8 continuation
    // bytes are always ≥ 0x80, so `0x0A` only ever appears as a real
    // line terminator (never inside a multi-byte sequence).
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break, // EOF — child closed PTY
            Ok(n) => n,
            Err(e) => {
                // EIO on Linux is the canonical "controlling tty went
                // away" signal once the child exits; treat as EOF.
                let kind = e.kind();
                if matches!(
                    kind,
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::UnexpectedEof
                ) || e.raw_os_error() == Some(5)
                {
                    break;
                }
                tracing::error!(pane_id = %pane_id, error = %e, "terminal read error");
                break;
            }
        };
        pending.extend_from_slice(&buf[..n]);

        // Intercept DSR-CPR queries (`\x1b[6n`) and respond. TUI apps
        // like `claude` send this at startup to ask the terminal where
        // its cursor is; they wait for an `\x1b[<row>;<col>R` reply
        // before painting *anything* (banner, prompt, the lot). A real
        // xterm replies automatically, but portable-pty + ConPTY's
        // pseudo-tty does not — the responsibility falls on whoever
        // owns the master end. Without this, claude under our PTY
        // emits 4 bytes (the query) and then sits idle forever.
        let dsr_count = extract_dsr_cpr_queries(&mut pending);
        if dsr_count > 0 {
            if let Ok(mut guard) = state.lock() {
                if let Some(w) = guard.writer.as_mut() {
                    for _ in 0..dsr_count {
                        let _ = w.write_all(b"\x1b[1;1R");
                    }
                    let _ = w.flush();
                }
            }
        }

        // Drain whole lines on each newline. Each line's bytes are
        // decoded once we have the full sequence — `from_utf8_lossy`
        // is now correct because no multi-byte char is split.
        while let Some(idx) = pending.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = pending.drain(..=idx).collect();
            let trimmed = trim_terminal_line_end(&line_bytes);
            emit_decoded_line(
                &app, &state, &pool, &pane_id, &agent_kind, trimmed, /* maybe_transition */ true,
            );
        }

        // L8: cap unflushed pending so a child emitting megabytes
        // without a newline cannot exhaust memory. Force-flush as a
        // partial line and continue. The flush stops at a UTF-8
        // boundary — a char split by the cap keeps its head bytes in
        // `pending` (flushed by the next newline / cap / EOF path)
        // instead of decoding U+FFFD on both sides of the cut.
        if pending.len() > MAX_PENDING_BYTES {
            let cut = utf8_safe_prefix_len(&pending);
            let forced: Vec<u8> = pending.drain(..cut).collect();
            let trimmed = trim_terminal_line_end(&forced);
            emit_decoded_line(
                &app, &state, &pool, &pane_id, &agent_kind, trimmed, /* maybe_transition */ true,
            );
        }
    }

    // Flush any trailing partial line (shells often emit a prompt
    // without a trailing newline). Treat as an `out` line so the
    // ring captures it. No status transition for the trailing flush
    // (the child has already exited; the waiter will set final status).
    if !pending.is_empty() {
        let trimmed = trim_terminal_line_end(&pending);
        if !trimmed.is_empty() {
            emit_decoded_line(
                &app, &state, &pool, &pane_id, &agent_kind, trimmed, /* maybe_transition */ false,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_decoded_line<R: Runtime>(
    app: &AppHandle<R>,
    state: &Arc<Mutex<PaneState>>,
    pool: &DbPool,
    pane_id: &str,
    agent_kind: &str,
    line_bytes: &[u8],
    transition_status: bool,
) {
    let text = String::from_utf8_lossy(line_bytes).into_owned();
    let text_for_db = strip_csi(&text);
    let payload = LineEventPayload {
        k: "out",
        text,
        seq: 0, // filled in below under the lock
    };
    emit_line(app, state, pane_id, payload, &text_for_db, "out");
    if transition_status {
        maybe_transition_status(state, app, pool, pane_id, agent_kind);
    }
}

/// Append one line to the ring under the per-pane mutex, allocate a
/// `seq`, and emit the corresponding `panes:{id}:line` event. Splitting
/// the seq allocation from the event emit ensures the seq the frontend
/// sees matches the seq we persisted.
fn emit_line<R: Runtime>(
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
fn maybe_transition_status<R: Runtime>(
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

// --------------------------------------------------------------------- //
// Waiter task — observes child exit, finalises DB state                //
// --------------------------------------------------------------------- //

pub(super) fn run_waiter<R: Runtime>(
    mut child: Box<dyn Child + Send + Sync>,
    state: Arc<Mutex<PaneState>>,
    app: AppHandle<R>,
    pool: DbPool,
    pane_id: String,
    registry: TerminalRegistry,
) {
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                // Once we observe a successful poll without exit, flip
                // STARTING → RUNNING so the frontend's "spawning" pill
                // resolves promptly. Idempotent.
                {
                    let mut guard = match state.lock() {
                        Ok(g) => g,
                        Err(_) => break portable_pty::ExitStatus::with_exit_code(1),
                    };
                    if guard.status == status::STARTING {
                        guard.status = status::RUNNING;
                        let pool = pool.clone();
                        let pane_id = pane_id.clone();
                        tauri::async_runtime::spawn(async move {
                            let _ = sqlx::query("UPDATE panes SET status = 'running' WHERE id = ?")
                                .bind(&pane_id)
                                .execute(&pool)
                                .await;
                        });
                    }
                }
                std::thread::sleep(WAIT_POLL);
            }
            Err(e) => {
                tracing::error!(pane_id = %pane_id, error = %e, "pane try_wait error");
                break portable_pty::ExitStatus::with_exit_code(1);
            }
        }
    };

    // Drop the writer + master so the reader task gets EOF. On
    // Windows ConPTY especially, the reader does not unblock until
    // every master/slave/writer reference is dropped — without this,
    // the `spawn_blocking` reader sits in `read()` indefinitely after
    // the child has already exited, leaking a thread and preventing
    // the registry from cleaning up the pane.
    {
        if let Ok(mut guard) = state.lock() {
            guard.writer.take();
            guard.master.take();
            guard.status = if exit_status.success() {
                status::SUCCESS
            } else {
                status::ERROR
            };
        }
    }

    // Synthesize a final `sys` line so the UI gets a definitive end
    // marker even if the shell printed nothing on its way out.
    let exit_text = format!("[exit {}]", exit_status.exit_code());
    let payload = LineEventPayload {
        k: "sys",
        text: exit_text.clone(),
        seq: 0,
    };
    emit_line(&app, &state, &pane_id, payload, &exit_text, "sys");

    let final_status = if exit_status.success() {
        status::SUCCESS
    } else {
        status::ERROR
    };

    // Flush ring → pane_lines and finalise panes.status/closed_at.
    let snapshot = {
        if let Ok(guard) = state.lock() {
            guard.ring.iter().cloned().collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };
    let pane_id_async = pane_id.clone();
    let pool_async = pool.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = flush_ring_to_db(&pool_async, &pane_id_async, &snapshot).await {
            tracing::error!(
                pane_id = %pane_id_async,
                error = %e,
                "terminal ring flush failed"
            );
        }
        let res = sqlx::query(
            "UPDATE panes SET status = ?, closed_at = strftime('%s','now') \
             WHERE id = ? AND closed_at IS NULL",
        )
        .bind(final_status)
        .bind(&pane_id_async)
        .execute(&pool_async)
        .await;
        if let Err(e) = res {
            tracing::error!(
                pane_id = %pane_id_async,
                error = %e,
                "pane finalise failed"
            );
        }
    });

    // Drop the registry slot so subsequent `pane_lines` reads fall
    // through to the DB.
    let registry_clone = registry.clone();
    let pane_id_for_drop = pane_id.clone();
    tauri::async_runtime::spawn(async move {
        let mut panes = registry_clone.inner.panes.lock().await;
        panes.remove(&pane_id_for_drop);
    });
}

pub(super) async fn flush_ring_to_db(
    pool: &DbPool,
    pane_id: &str,
    lines: &[RingLine],
) -> Result<(), AppError> {
    if lines.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for line in lines {
        // OR IGNORE + the UNIQUE(pane_id, seq) index (migration 0012)
        // make the flush idempotent — the waiter finalise and app-exit
        // shutdown_all can both snapshot the same ring on a close race.
        sqlx::query(
            "INSERT OR IGNORE INTO pane_lines (pane_id, seq, k, text) VALUES (?, ?, ?, ?)",
        )
        .bind(pane_id)
        .bind(line.seq)
        .bind(line.kind)
        .bind(&line.text)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

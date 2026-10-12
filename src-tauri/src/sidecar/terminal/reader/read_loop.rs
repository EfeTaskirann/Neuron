//! Reader task — turns PTY bytes into events + ring lines.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Runtime};

use crate::db::DbPool;
use crate::tuning::{MAX_PENDING_BYTES, READ_CHUNK_BYTES};

use super::super::text::{
    extract_dsr_cpr_queries, strip_csi, trim_terminal_line_end, utf8_safe_prefix_len,
};
use super::super::PaneState;
use super::emit::{emit_data, emit_line, LineEventPayload};
use super::transition::maybe_transition_status;

pub(in crate::sidecar::terminal) fn run_reader<R: Runtime>(
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
    // Separate carry for the raw interactive stream (`panes:{id}:data`).
    // Carries the same bytes as `pending` but is never line-split: it is
    // emitted verbatim to xterm so keystroke echo, cursor moves, and
    // ANSI colour render live (the line channel below only surfaces on
    // `\n`). Only an incomplete trailing UTF-8 char is held back between
    // reads (≤3 bytes), so it never grows unbounded.
    let mut raw_pending: Vec<u8> = Vec::new();
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

        // Raw interactive passthrough. Emit this read's bytes verbatim
        // on `panes:{id}:data` so xterm renders keystroke echo, cursor
        // moves, and ANSI colour immediately — the line channel below
        // only surfaces on `\n`, which is why typed input used to stay
        // invisible until Enter. DSR-CPR queries are stripped here too
        // (already answered above via `pending`); forwarding them to
        // xterm would trigger a duplicate cursor-position reply.
        raw_pending.extend_from_slice(&buf[..n]);
        let _ = extract_dsr_cpr_queries(&mut raw_pending);
        let cut = utf8_safe_prefix_len(&raw_pending);
        if cut > 0 {
            let chunk: Vec<u8> = raw_pending.drain(..cut).collect();
            emit_data(&app, &pane_id, &String::from_utf8_lossy(&chunk));
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

    // Flush the raw stream's held-back tail (an incomplete UTF-8 char,
    // or a final chunk with no newline) so the last keystrokes / prompt
    // reach xterm before the pane goes dead.
    if !raw_pending.is_empty() {
        emit_data(&app, &pane_id, &String::from_utf8_lossy(&raw_pending));
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

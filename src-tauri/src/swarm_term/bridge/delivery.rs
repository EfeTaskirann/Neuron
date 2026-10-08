//! Delivery mechanics: the pane-status gate, the timed route-write, and
//! the bracketed-paste wire format pasted into a target pane's stdin.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::error::AppError;
use crate::sidecar::terminal::TerminalRegistry;

/// Hard cap on one paste-then-submit (both PTY writes + the submit
/// delay). claude's bracketed-paste accept is sub-ms in healthy
/// conditions; a 2 s cap surfaces stuck panes within one tick of the
/// user noticing nothing happens.
pub(super) const WRITE_TIMEOUT_SECS: u64 = 2;

/// Gap between a bracketed paste and the `\r` that submits it. A CR
/// arriving in the same PTY write as the paste can be absorbed by
/// claude's TUI as part of the paste, leaving the text sitting unsent
/// in the input box; a separate, slightly later write is a distinct
/// keypress.
pub(crate) const SUBMIT_DELAY_MS: u64 = 200;

/// Write a complete bracketed-paste block (no trailing CR) into `pane`,
/// wait [`SUBMIT_DELAY_MS`], then submit it with a lone `\r`. Every
/// paste into a swarm-term claude pane goes through here: routed
/// messages, `[Neuron]` notices, persona injection, the auto-prompt.
pub(crate) async fn paste_then_submit(
    registry: &TerminalRegistry,
    pane: &str,
    paste: &[u8],
) -> Result<(), AppError> {
    registry.write_to_pane(pane, paste).await?;
    tokio::time::sleep(Duration::from_millis(SUBMIT_DELAY_MS)).await;
    registry.write_to_pane(pane, b"\r").await
}

/// True when a pane's status means a routed message must NOT be
/// delivered right now: only `error` (the pane is wedged / has no
/// stdin). A `success` / idle pane (last turn finished, prompt ready) IS
/// a valid delivery target — including it here previously starved
/// lifecycle fanouts to just-finished reviewer/orchestrator panes.
///
/// `awaiting_approval` is deliberately NOT blocking: swarm-term panes
/// run with `--permission-mode bypassPermissions`, so claude never shows
/// a real approval prompt there. The status can only come from the
/// generic `Approve.*?` regex matching the agent's own output, and it
/// is never cleared — blocking on it froze the pane's inbox for good.
pub(super) fn is_delivery_blocked(status: Option<&str>) -> bool {
    matches!(status, Some("error"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteWriteOutcome {
    Ok,
    Timeout,
    Err,
}

pub(super) async fn perform_route_write_with_timeout<R, F>(
    write_fut: F,
    app: AppHandle<R>,
    source: String,
    target: String,
    target_pane: String,
) -> RouteWriteOutcome
where
    R: Runtime,
    F: std::future::Future<Output = Result<(), AppError>>,
{
    let result = tokio::time::timeout(
        Duration::from_secs(WRITE_TIMEOUT_SECS),
        write_fut,
    )
    .await;
    match result {
        Err(_) => {
            tracing::warn!(
                source = %source,
                target = %target,
                target_pane = %target_pane,
                timeout_secs = WRITE_TIMEOUT_SECS,
                "swarm-term bridge: route write timed out"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": source,
                    "target": target,
                    "outcome": "target_write_timeout",
                    "timeout_secs": WRITE_TIMEOUT_SECS,
                }),
            );
            RouteWriteOutcome::Timeout
        }
        Ok(Err(e)) => {
            tracing::warn!(
                source = %source,
                target = %target,
                target_pane = %target_pane,
                error = %e,
                "swarm-term bridge: route write failed"
            );
            RouteWriteOutcome::Err
        }
        Ok(Ok(())) => RouteWriteOutcome::Ok,
    }
}

/// Wire-format the routed payload that's pasted into the target pane's
/// stdin:
///
///   * `\x1b[200~ … \x1b[201~` — xterm bracketed paste. The submitting
///     CR is NOT included; [`paste_then_submit`] sends it separately.
///   * `\r` inside body → `\n` (CR inside paste = visual overstrike).
///   * Embedded `\x1b[201~` → `[201~` (ESC byte dropped) so the
///     bracketed paste isn't prematurely closed by the body content.
pub(crate) fn format_routed_message(body: &str, source_agent: &str) -> String {
    let safe = body
        .replace('\r', "\n")
        .replace("\x1b[201~", "[201~");
    format!(
        "\x1b[200~{safe}\n\n— from @{source_agent} [routed by Neuron]\x1b[201~"
    )
}

/// Wire-format a Neuron-originated notice (delivery failure, unparsed
/// lifecycle signal) pasted into an agent's pane. Same bracketed-paste
/// framing and sanitising as [`format_routed_message`], but signed as
/// coming from the bridge itself so the agent does not reply to it as
/// if it were a peer's message.
pub(crate) fn format_system_notice(text: &str) -> String {
    let safe = text
        .replace('\r', "\n")
        .replace("\x1b[201~", "[201~");
    format!("\x1b[200~[Neuron] {safe}\x1b[201~")
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

//! Delivery mechanics: the pane-status gate, the timed route-write, and
//! the bracketed-paste wire format pasted into a target pane's stdin.

use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::error::AppError;
use crate::sidecar::terminal::TerminalRegistry;

/// Hard cap on a single PTY write (the routed paste, or its submitting
/// CR). claude's bracketed-paste accept is sub-ms in healthy
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
    submit_pasted(registry, pane).await
}

/// Second half of [`paste_then_submit`]: wait [`SUBMIT_DELAY_MS`], then
/// send the lone `\r` (bounded by [`WRITE_TIMEOUT_SECS`]). Exposed so a
/// caller that retries on failure can retry the PASTE alone — once the
/// paste has landed, re-running the whole sequence would paste the same
/// text into the input box a second time.
pub(crate) async fn submit_pasted(
    registry: &TerminalRegistry,
    pane: &str,
) -> Result<(), AppError> {
    tokio::time::sleep(Duration::from_millis(SUBMIT_DELAY_MS)).await;
    match tokio::time::timeout(
        Duration::from_secs(WRITE_TIMEOUT_SECS),
        registry.write_to_pane(pane, b"\r"),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(AppError::Internal(format!(
            "submit CR to pane {pane} timed out"
        ))),
    }
}

/// True when a pane's status means a routed message must NOT be
/// delivered right now: only `error` (the pane is wedged / has no
/// stdin). A `success` / idle pane (last turn finished, prompt ready) IS
/// a valid delivery target — including it here previously starved
/// lifecycle fanouts to just-finished reviewer/orchestrator panes.
///
/// `awaiting_approval` is deliberately NOT blocking: swarm-term panes
/// run with `--permission-mode dontAsk` (or `bypassPermissions` when
/// opted out — see `swarm_term::permissions`); neither ever shows a real
/// approval prompt — a non-allowed tool is denied, not asked. The status
/// can only come from the
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

/// Make arbitrary text safe to place INSIDE a bracketed paste: CR / CRLF
/// become LF, and every other control character — the whole C0 range
/// including ESC, DEL, and the C1 range (U+0080–U+009F, e.g. the 8-bit
/// CSI) — is removed; only LF and TAB survive.
///
/// Removing the ESC byte itself (instead of replacing the `\x1b[201~`
/// paste-end sequence) is what makes this sound: a one-pass `replace`
/// of the sequence re-forms it from `\x1b\x1b[201~`, closing the paste
/// early so the rest of the text reaches claude's TUI as keystrokes
/// (Esc cancels the turn, Ctrl-C ×2 kills the pane, Shift-Tab flips the
/// mode). Without ESC no terminal sequence can be formed at all.
pub(crate) fn sanitize_for_paste(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\r' { '\n' } else { c })
        .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
        .collect()
}

/// Wire-format the routed payload that's pasted into the target pane's
/// stdin:
///
///   * `\x1b[200~ … \x1b[201~` — xterm bracketed paste. The submitting
///     CR is NOT included; [`paste_then_submit`] sends it separately.
///   * The body goes through [`sanitize_for_paste`], so it cannot close
///     the paste early or carry terminal control sequences.
///   * The envelope's structured `task_id`, when present, is shown in
///     the signature (`— from @x [task T1] [routed by Neuron]`). Only the
///     body used to be pasted, so a receiver could not see the id the
///     sender had filled in (observed: a coordinator concluding a
///     message had "no task_id").
pub(crate) fn format_routed_message(
    body: &str,
    source_agent: &str,
    task_id: Option<&str>,
) -> String {
    let safe = neutralize_bridge_markers(&sanitize_for_paste(body));
    // A body whose first non-space character is a claude REPL trigger —
    // `/` (slash command), `!` (bash), `#` (memory) — would act on the
    // shared session when this is pasted and submitted. A zero-width space
    // in front keeps it from being the first character; it is invisible to
    // the agent and survives `sanitize_for_paste` (U+200B is a format
    // char, not a control one), and the routed signature appended below
    // makes the whole paste multi-line regardless.
    let safe = if safe.trim_start().starts_with(['/', '!', '#']) {
        format!("\u{200B}{safe}")
    } else {
        safe
    };
    let task = task_id
        .map(|id| {
            let printable: String = id.chars().filter(|c| !c.is_control()).collect();
            crate::text::truncate_chars(&printable, MAX_SIGNATURE_TASK_ID_CHARS)
        })
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!(" [task {id}]"))
        .unwrap_or_default();
    format!(
        "\x1b[200~{safe}\n\n— from @{source_agent}{task} [routed by Neuron]\x1b[201~"
    )
}

/// Defuse the bridge's own markers inside a peer's body: `[routed by
/// Neuron]` (the routed signature) and `[Neuron]` (the bridge-notice
/// prefix the persona tells agents to obey) become `(routed by Neuron)` /
/// `(Neuron)`. Otherwise a body could carry a fake `— from @orchestrator
/// [routed by Neuron]` line or pose as a bridge notice. The genuine
/// signature is appended after this, so it is always the LAST line.
fn neutralize_bridge_markers(body: &str) -> String {
    body.replace("[routed by Neuron]", "(routed by Neuron)")
        .replace("[Neuron]", "(Neuron)")
}

/// Cap on the task id echoed in the routed signature.
const MAX_SIGNATURE_TASK_ID_CHARS: usize = 64;

/// Wire-format a Neuron-originated notice (delivery failure, unparsed
/// lifecycle signal) pasted into an agent's pane. Same bracketed-paste
/// framing and sanitising as [`format_routed_message`], but signed as
/// coming from the bridge itself so the agent does not reply to it as
/// if it were a peer's message.
pub(crate) fn format_system_notice(text: &str) -> String {
    let safe = sanitize_for_paste(text);
    format!("\x1b[200~[Neuron] {safe}\x1b[201~")
}

/// `s` capped at `max` bytes (on a char boundary), with `…` when cut.
pub(super) fn truncate(s: &str, max: usize) -> String {
    let cut = crate::text::truncate_to_char_boundary(s, max);
    if cut.len() == s.len() {
        s.to_string()
    } else {
        format!("{cut}…")
    }
}

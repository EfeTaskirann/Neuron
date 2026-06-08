//! Stdout read loop for a persistent `claude` session.
//!
//! `read_until_result` drives one turn's worth of stream-json parsing
//! (racing cancel + per-turn timeout); `drain_post_cancel` flushes
//! leftover bytes so framing stays aligned for the next turn;
//! `InvokeAccum` is the per-turn running state. Driven by
//! `super::session::PersistentSession::invoke_turn`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::transport::{classify_event, InvokeResult, StreamEvent};

use super::event::TurnStreamEvent;

/// Best-effort drain budget after a cancel signal: read up to this
/// many bytes from stdout to preserve framing for the next turn.
/// 4 KiB absorbs the typical mid-turn `result` event without blocking
/// the cancel return for too long.
const POST_CANCEL_DRAIN_BUDGET_BYTES: usize = 4 * 1024;

/// Hard ceiling on the post-cancel drain wall time. Even if the budget
/// hasn't been hit, return after this much time so `Cancelled` is
/// observed promptly.
const POST_CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Read stream-json events until the next `result` (success or error)
/// or until cancel / timeout / stdout EOF fires.
///
/// Pub-within-package so the caller (`PersistentSession::invoke_turn`)
/// can drive it directly. The shape mirrors the inner read_loop in
/// `SubprocessTransport::invoke` — one of three reasons we don't
/// extract a shared free fn yet:
///
/// 1. The one-shot path's read loop is a pure subset (no cancel arm).
/// 2. The one-shot path returns `Result<(), AppError>` and pumps an
///    outer `InvokeAccum`; this path returns `Result<InvokeResult>`
///    directly. Different shapes.
/// 3. Sharing would require either a `Notify` that never fires for
///    one-shot (extra plumbing) or a dyn trait split. Not worth it
///    for ~30 lines of duplication.
///
/// If a third caller arrives we'll factor; for W4-01 the duplication
/// is the right size.
pub(super) async fn read_until_result(
    reader: &mut BufReader<ChildStdout>,
    timeout: Duration,
    cancel: Option<Arc<Notify>>,
    event_sink: Option<&UnboundedSender<TurnStreamEvent>>,
) -> Result<InvokeResult, AppError> {
    let mut accum = InvokeAccum::default();
    let read_loop = async {
        let mut lines = reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                line = %line,
                                "persistent session: stdout line is not JSON"
                            );
                            continue;
                        }
                    };
                    for ev in classify_event(&value) {
                        match ev {
                            StreamEvent::SystemInit { session_id } => {
                                accum.session_id = Some(session_id);
                            }
                            StreamEvent::AssistantDelta { text } => {
                                accum.assistant_text.push_str(&text);
                                if let Some(tx) = event_sink {
                                    let _ = tx.send(
                                        TurnStreamEvent::AssistantText {
                                            delta: text.clone(),
                                        },
                                    );
                                }
                            }
                            StreamEvent::ToolUse {
                                name,
                                input_summary,
                            } => {
                                if let Some(tx) = event_sink {
                                    let _ = tx.send(
                                        TurnStreamEvent::ToolUse {
                                            name: name.clone(),
                                            input_summary:
                                                input_summary.clone(),
                                        },
                                    );
                                }
                            }
                            StreamEvent::ResultSuccess {
                                assistant_text,
                                total_cost_usd,
                                turn_count,
                            } => {
                                return Ok(InvokeResult {
                                    session_id: accum
                                        .session_id
                                        .clone()
                                        .unwrap_or_default(),
                                    assistant_text: if !assistant_text
                                        .is_empty()
                                    {
                                        assistant_text
                                    } else {
                                        accum.assistant_text.clone()
                                    },
                                    total_cost_usd,
                                    turn_count,
                                });
                            }
                            StreamEvent::ResultError { reason } => {
                                return Err(AppError::SwarmInvoke(format!(
                                    "swarm invoke error: {reason}"
                                )));
                            }
                            StreamEvent::Other => {}
                        }
                    }
                }
                Ok(None) => {
                    return Err(AppError::SwarmInvoke(
                        "claude stdout closed before `result` event \
                         (child may have crashed)"
                            .into(),
                    ));
                }
                Err(e) => {
                    return Err(AppError::SwarmInvoke(format!(
                        "stdout read error: {e}"
                    )));
                }
            }
        }
    };

    // Race the read loop against optional cancel + the per-turn
    // timeout. `tokio::time::timeout` is the outermost wrapper so
    // either branch (cancel arm OR read finishing OR timeout)
    // unblocks promptly.
    match cancel {
        Some(notify) => {
            let cancel_arm = async {
                notify.notified().await;
                Err::<InvokeResult, AppError>(AppError::Cancelled(
                    "cancel notify fired".into(),
                ))
            };
            tokio::select! {
                outcome = tokio::time::timeout(timeout, read_loop) => {
                    match outcome {
                        Err(_elapsed) => Err(AppError::Timeout(format!(
                            "claude subprocess did not produce a `result` \
                             event within {timeout:?}"
                        ))),
                        Ok(Ok(result)) => Ok(result),
                        Ok(Err(e)) => Err(e),
                    }
                }
                cancelled = cancel_arm => cancelled,
            }
        }
        None => match tokio::time::timeout(timeout, read_loop).await {
            Err(_elapsed) => Err(AppError::Timeout(format!(
                "claude subprocess did not produce a `result` event \
                 within {timeout:?}"
            ))),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(e),
        },
    }
}

/// Best-effort drain after a cancel: read off stdout for up to
/// `POST_CANCEL_DRAIN_BUDGET_BYTES` bytes / `POST_CANCEL_DRAIN_TIMEOUT`
/// wall time so leftover stream-json events don't show up at the
/// start of the next turn.
pub(super) async fn drain_post_cancel(reader: &mut BufReader<ChildStdout>) {
    let drain_loop = async {
        let mut total = 0usize;
        let mut tmp = [0u8; 1024];
        loop {
            match tokio::io::AsyncReadExt::read(reader, &mut tmp).await {
                Ok(0) => return,
                Ok(n) => {
                    total = total.saturating_add(n);
                    if total >= POST_CANCEL_DRAIN_BUDGET_BYTES {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    };
    let _ = tokio::time::timeout(POST_CANCEL_DRAIN_TIMEOUT, drain_loop).await;
}

/// Mirror of the private `InvokeAccum` in `transport.rs`. Kept local
/// to this module to avoid widening the transport.rs visibility for
/// what is, conceptually, just running state of one read scope.
#[derive(Default)]
pub(super) struct InvokeAccum {
    pub(super) session_id: Option<String>,
    pub(super) assistant_text: String,
}

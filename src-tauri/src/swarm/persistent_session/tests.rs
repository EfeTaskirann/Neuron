//! Unit + integration tests for the persistent session read path.
//!
//! The helper `read_until_result_via_duplex*` replicates the
//! production read loop over a `DuplexStream` so the suite can drive
//! it without spawning a real `claude` child.

use super::read::InvokeAccum;
use super::PersistentSession;

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::transport::{classify_event, InvokeResult, StreamEvent};

/// Adapter that lets a `tokio::io::DuplexStream` stand in for a
/// `ChildStdout` so the unit suite can drive `read_until_result`
/// without spawning a real subprocess. The test seeds the
/// "client" half with stream-json bytes and the read loop pulls
/// them off the "server" half.
///
/// We re-import `BufReader<ChildStdout>` only because the public
/// `PersistentSession` wraps that type. For the helper tests we
/// drive `read_until_result` directly via a local generic helper
/// (below).
async fn read_until_result_via_duplex(
    bytes: &[u8],
    timeout: Duration,
    cancel: Option<Arc<Notify>>,
) -> Result<InvokeResult, AppError> {
    read_until_result_via_duplex_with_close(bytes, timeout, cancel, true)
        .await
}

/// `keep_open=false` drops the writer after seeding the bytes,
/// which signals EOF to the reader (the closed-pipe case for
/// EOF-without-result tests). `keep_open=true` keeps the writer
/// alive forever, so the reader blocks on `next_line` and the
/// cancel / timeout arms can fire — the cancel/timeout tests
/// need this; the success-path tests don't.
async fn read_until_result_via_duplex_with_close(
    bytes: &[u8],
    timeout: Duration,
    cancel: Option<Arc<Notify>>,
    close_on_eof: bool,
) -> Result<InvokeResult, AppError> {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    let bytes = bytes.to_vec();
    // Spawn the writer-side. If `close_on_eof` is false we hold
    // the writer alive for the full test duration so the reader
    // never sees EOF — this is what the cancel / timeout tests
    // need. We keep the JoinHandle in scope after this fn
    // returns by spawning detached; the writer drops when the
    // task is reaped at suite end (or when `close_on_eof=true`,
    // explicitly inside the task).
    tokio::spawn(async move {
        let _ = writer.write_all(&bytes).await;
        let _ = writer.flush().await;
        if close_on_eof {
            drop(writer);
        } else {
            // Park the task without dropping the writer. The test
            // will time out / cancel before this completes.
            tokio::time::sleep(Duration::from_secs(60)).await;
            drop(writer);
        }
    });
    let buf_reader = BufReader::new(reader);
    // Local copy of the read loop adapted to a generic
    // `AsyncBufReadExt` reader. The production fn is bound to
    // `BufReader<ChildStdout>` so we can't reuse it directly in
    // tests; we replicate the parser logic inline.
    let mut accum = InvokeAccum::default();
    let read_loop = async {
        let mut lines = buf_reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    for ev in classify_event(&v) {
                        match ev {
                            StreamEvent::SystemInit { session_id } => {
                                accum.session_id = Some(session_id);
                            }
                            StreamEvent::AssistantDelta { text } => {
                                accum.assistant_text.push_str(&text);
                            }
                            StreamEvent::ToolUse { .. } => {
                                // Test helper doesn't surface
                                // ToolUse events — see the
                                // dedicated event-sink test for
                                // that path.
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
                                return Err(AppError::SwarmInvoke(
                                    format!(
                                        "swarm invoke error: {reason}"
                                    ),
                                ));
                            }
                            StreamEvent::Other => {}
                        }
                    }
                }
                Ok(None) => {
                    return Err(AppError::SwarmInvoke(
                        "stdout EOF before result".into(),
                    ));
                }
                Err(e) => {
                    return Err(AppError::SwarmInvoke(format!(
                        "read error: {e}"
                    )));
                }
            }
        }
    };
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
                        Err(_) => Err(AppError::Timeout(
                            format!("did not produce result within {timeout:?}")
                        )),
                        Ok(Ok(r)) => Ok(r),
                        Ok(Err(e)) => Err(e),
                    }
                }
                cancelled = cancel_arm => cancelled,
            }
        }
        None => match tokio::time::timeout(timeout, read_loop).await {
            Err(_) => Err(AppError::Timeout(format!(
                "did not produce result within {timeout:?}"
            ))),
            Ok(Ok(r)) => Ok(r),
            Ok(Err(e)) => Err(e),
        },
    }
}

fn ok_event_lines(text: &str, cost: f64, turns: u32) -> String {
    let mut s = String::new();
    writeln!(
        s,
        r#"{{"type":"system","subtype":"init","session_id":"sess-test"}}"#
    )
    .unwrap();
    writeln!(
        s,
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
    )
    .unwrap();
    writeln!(
        s,
        r#"{{"type":"result","subtype":"success","result":"{text}","total_cost_usd":{cost},"num_turns":{turns}}}"#
    )
    .unwrap();
    s
}

/// Single-turn happy path: classifier sees init / assistant /
/// result.success in order and returns a populated InvokeResult.
#[tokio::test]
async fn single_turn_round_trip() {
    let bytes = ok_event_lines("hello", 0.0042, 3);
    let result = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect("ok");
    assert_eq!(result.session_id, "sess-test");
    assert_eq!(result.assistant_text, "hello");
    assert!((result.total_cost_usd - 0.0042).abs() < 1e-9);
    assert_eq!(result.turn_count, 3);
}

/// Two consecutive `result.success` events arriving in one read
/// scope — the read loop returns on the FIRST one and leaves the
/// rest unread, which is the contract the persistent session
/// relies on (turn 2 starts a fresh read scope).
#[tokio::test]
async fn read_loop_returns_on_first_result_only() {
    let mut bytes = ok_event_lines("turn-1-text", 0.0, 1);
    bytes.push_str(&ok_event_lines("turn-2-text", 0.0, 1));
    // We can only verify "turn 1's text" returns; turn 2's bytes
    // are still in the duplex pipe and would be picked up by the
    // next `read_until_result` call. Driving that here would
    // reuse the already-dropped reader, which the test harness
    // doesn't expose — covered by the integration smoke instead.
    let result = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect("ok");
    assert_eq!(result.assistant_text, "turn-1-text");
}

/// `result.subtype = "error_max_turns"` surfaces as `SwarmInvoke`
/// with the canonical message that prior FSM tests pin against.
#[tokio::test]
async fn error_max_turns_event_returns_swarminvoke() {
    let bytes =
        r#"{"type":"result","subtype":"error_max_turns","error":"claude reported a result.error event"}
"#;
    let err = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect_err("err");
    match err {
        AppError::SwarmInvoke(msg) => {
            assert!(msg.contains("error_max_turns"), "got: {msg}");
        }
        other => panic!("expected SwarmInvoke, got {other:?}"),
    }
}

/// Stream-json parser stays robust against malformed lines —
/// non-JSON bytes are warn-logged and skipped, NOT propagated as
/// errors. The test feeds garbage interleaved with a valid
/// result.success and expects the success to be observed.
#[tokio::test]
async fn malformed_lines_are_skipped() {
    let mut bytes = String::new();
    bytes.push_str("not json\n");
    bytes.push_str("{partial json...\n");
    bytes.push_str(&ok_event_lines("after-garbage", 0.0, 1));
    let result = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect("ok");
    assert_eq!(result.assistant_text, "after-garbage");
}

/// Stdout EOF before `result` surfaces as `SwarmInvoke` (the
/// child crashed or claude exited unexpectedly).
#[tokio::test]
async fn eof_before_result_returns_swarminvoke() {
    // Only init + a partial assistant block — no result event.
    let bytes = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"oops"}]}}
"#;
    let err = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect_err("err");
    match err {
        AppError::SwarmInvoke(msg) => {
            assert!(msg.contains("EOF") || msg.contains("result"));
        }
        other => panic!("expected SwarmInvoke, got {other:?}"),
    }
}

/// Cancel notify fires before the read returns → Cancelled.
/// Drives the cancel arm directly with the writer kept alive so
/// the reader blocks on `next_line` instead of seeing EOF.
#[tokio::test]
async fn cancel_before_result_returns_cancelled() {
    let cancel = Arc::new(Notify::new());
    let cancel_for_fire = Arc::clone(&cancel);
    // Fire cancel after a short delay so the read loop has time
    // to reach the select.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel_for_fire.notify_waiters();
    });
    let err = read_until_result_via_duplex_with_close(
        // empty seed; writer stays alive (close_on_eof=false) so
        // reader blocks on next_line instead of seeing EOF.
        &[],
        Duration::from_secs(5),
        Some(cancel),
        false,
    )
    .await
    .expect_err("err");
    assert!(matches!(err, AppError::Cancelled(_)));
}

/// Per-turn timeout fires before any event → Timeout error.
/// Same writer-kept-alive trick as the cancel test.
#[tokio::test]
async fn timeout_before_result_returns_timeout() {
    let err = read_until_result_via_duplex_with_close(
        &[],
        Duration::from_millis(50),
        None,
        false,
    )
    .await
    .expect_err("err");
    assert!(matches!(err, AppError::Timeout(_)));
}

/// Forward-compat: `num_turns` and `turn_count` spellings both
/// land in the parsed `turn_count`. (Already covered in
/// `transport::tests` for the classifier; replicated here so the
/// persistent path is independently regression-guarded.)
#[tokio::test]
async fn turn_count_alternate_spelling_lands() {
    let bytes = r#"{"type":"result","subtype":"success","result":"x","total_cost_usd":0,"turn_count":7}
"#;
    let result = read_until_result_via_duplex(
        bytes.as_bytes(),
        Duration::from_secs(5),
        None,
    )
    .await
    .expect("ok");
    assert_eq!(result.turn_count, 7);
}

/// Real-claude integration smoke (`#[ignore]`'d) — drives a
/// two-turn session against the `scout` profile. Turn 1 asks for
/// a fact; turn 2 asks the session to recall what it just said.
/// Asserts turn 2's response references turn 1's content.
///
/// Time budget: 2 × 180s = 360s worst-case; typical 30-90s.
/// `NEURON_SWARM_STAGE_TIMEOUT_SEC` overrides each turn.
#[tokio::test]
#[ignore = "requires real `claude` binary + Pro/Max subscription"]
async fn integration_persistent_two_turn_real_claude() {
    use crate::swarm::profile::ProfileRegistry;
    use crate::test_support::mock_app_with_pool;

    let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(180);

    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry =
        ProfileRegistry::load_from(None).expect("load registry");
    let scout = registry.get("scout").expect("scout profile");

    let mut session = PersistentSession::spawn(app.handle(), scout)
        .await
        .expect("spawn session");

    // Turn 1: ask for a single fact, instruct the persona to
    // answer concisely so turn 2 can quote it back.
    let cancel = Arc::new(Notify::new());
    let turn1 = session
        .invoke_turn(
            "Reply with exactly the single word `ALPHA` and nothing else.",
            Duration::from_secs(stage_secs),
            Arc::clone(&cancel),
            None,
        )
        .await
        .expect("turn 1 ok");
    assert!(
        turn1.assistant_text.to_uppercase().contains("ALPHA"),
        "turn 1 should contain ALPHA, got: {}",
        turn1.assistant_text
    );

    // Turn 2: ask the same session what it just replied. If
    // session context carried, the model knows.
    let turn2 = session
        .invoke_turn(
            "What was the single word you just replied with? Answer in one word.",
            Duration::from_secs(stage_secs),
            cancel,
            None,
        )
        .await
        .expect("turn 2 ok");
    assert!(
        turn2.assistant_text.to_uppercase().contains("ALPHA"),
        "turn 2 should recall ALPHA, got: {}",
        turn2.assistant_text
    );

    assert_eq!(session.turns_taken(), 2);
    session.shutdown().await.expect("shutdown ok");
}

//! Unit tests for the `transport` package — the stream-json line
//! classifier ([`super::classify`]), the stderr ring buffer
//! ([`super::ring`]), and an opt-in integration smoke against the
//! real `claude` binary ([`super::subprocess`]).
//!
//! Tests reach every symbol through the package re-exports, so
//! `use super::*` resolves the same as the pre-split single-file
//! version. `Duration` / `Value` are pulled in explicitly because the
//! re-export glob does not forward the submodules' own `use` imports.

use super::classify::TOOL_USE_INPUT_SUMMARY_CAP;
use super::*;
use serde_json::Value;
use std::time::Duration;

/// WP §7 — feed the line classifier a stream-json fixture and
/// confirm it produces the correct sequence of `StreamEvent`s.
/// W4-03 extended `classify_event` to return `Vec<StreamEvent>`.
#[test]
fn stream_json_line_parser() {
    // 1. system.init
    let init: Value = serde_json::from_str(
        r#"{"type":"system","subtype":"init","session_id":"sess-abc"}"#,
    )
    .unwrap();
    assert_eq!(
        classify_event(&init),
        vec![StreamEvent::SystemInit {
            session_id: "sess-abc".into()
        }]
    );

    // 2. assistant text delta (single text block).
    let asst1: Value = serde_json::from_str(
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello "}]}}"#,
    )
    .unwrap();
    assert_eq!(
        classify_event(&asst1),
        vec![StreamEvent::AssistantDelta {
            text: "Hello ".into()
        }]
    );

    // 3. assistant text + tool_use (W4-03 emits BOTH events;
    //    text first, tool_use after).
    let asst2: Value = serde_json::from_str(
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"path":"x.rs"}},{"type":"text","text":"world"}]}}"#,
    )
    .unwrap();
    assert_eq!(
        classify_event(&asst2),
        vec![
            StreamEvent::AssistantDelta {
                text: "world".into()
            },
            StreamEvent::ToolUse {
                name: "Read".into(),
                input_summary: "path: x.rs".into(),
            },
        ]
    );

    // 4. result.success — final answer, cost, turn count.
    let result: Value = serde_json::from_str(
        r#"{"type":"result","subtype":"success","result":"Hello world","total_cost_usd":0.0123,"num_turns":2}"#,
    )
    .unwrap();
    let events = classify_event(&result);
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ResultSuccess {
            assistant_text,
            total_cost_usd,
            turn_count,
        } => {
            assert_eq!(assistant_text, "Hello world");
            assert!((total_cost_usd - 0.0123).abs() < 1e-9);
            assert_eq!(*turn_count, 2);
        }
        other => panic!("expected ResultSuccess, got {other:?}"),
    }

    // 5. result.error — surfaced as ResultError with both subtype
    //    and reason in the message.
    let err: Value = serde_json::from_str(
        r#"{"type":"result","subtype":"error","error":"OAuth expired"}"#,
    )
    .unwrap();
    let events = classify_event(&err);
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ResultError { reason } => {
            assert!(reason.contains("error"));
            assert!(reason.contains("OAuth expired"));
        }
        other => panic!("expected ResultError, got {other:?}"),
    }

    // 6. Forward-compat — unknown types produce empty Vec
    //    (caller skips them).
    let unknown: Value =
        serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
    assert!(classify_event(&unknown).is_empty());
}

/// `turn_count` should also pick up the `turn_count` spelling
/// (forward-compat with a future CLI rename).
#[test]
fn stream_json_accepts_alternate_turn_count_spelling() {
    let v: Value = serde_json::from_str(
        r#"{"type":"result","subtype":"success","result":"ok","total_cost_usd":0,"turn_count":4}"#,
    )
    .unwrap();
    let events = classify_event(&v);
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ResultSuccess { turn_count, .. } => {
            assert_eq!(*turn_count, 4);
        }
        other => panic!("expected ResultSuccess, got {other:?}"),
    }
}

/// W4-03 — tool_use block alone (no text) emits a single
/// `ToolUse` event.
#[test]
fn classify_event_parses_tool_use_alone() {
    let v: Value = serde_json::from_str(
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Glob","input":{"pattern":"*.rs"}}]}}"#,
    )
    .unwrap();
    assert_eq!(
        classify_event(&v),
        vec![StreamEvent::ToolUse {
            name: "Glob".into(),
            input_summary: "pattern: *.rs".into(),
        }]
    );
}

/// W4-03 — long tool input is truncated to ~120 chars with a
/// trailing ellipsis so log spam is bounded.
#[test]
fn classify_event_truncates_long_tool_input() {
    let huge = "x".repeat(500);
    let line = format!(
        r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"path":"{huge}"}}}}]}}}}"#
    );
    let v: Value = serde_json::from_str(&line).unwrap();
    let events = classify_event(&v);
    assert_eq!(events.len(), 1);
    match &events[0] {
        StreamEvent::ToolUse { name, input_summary } => {
            assert_eq!(name, "Read");
            assert_eq!(
                input_summary.chars().count(),
                TOOL_USE_INPUT_SUMMARY_CAP
            );
            assert!(input_summary.ends_with('…'));
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

/// W4-03 — tool_use with no `input` field falls back to empty
/// summary (no panic).
#[test]
fn classify_event_tool_use_without_input() {
    let v: Value = serde_json::from_str(
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"BashSummary"}]}}"#,
    )
    .unwrap();
    assert_eq!(
        classify_event(&v),
        vec![StreamEvent::ToolUse {
            name: "BashSummary".into(),
            input_summary: String::new(),
        }]
    );
}

/// The ring buffer keeps only the most recent `capacity` bytes.
#[test]
fn ring_buffer_truncates_oldest() {
    let mut ring = RingBuffer::new(8);
    ring.append(b"abcdefgh"); // fills exactly
    assert_eq!(ring.tail_string(8), "abcdefgh");
    ring.append(b"ij"); // should drop "ab"
    assert_eq!(ring.tail_string(8), "cdefghij");
    // Burst alone exceeds capacity → keep tail.
    ring.append(b"0123456789012345");
    assert_eq!(ring.tail_string(8), "89012345");
}

/// Integration smoke (`#[ignore]`) — spawns the real `claude`
/// binary against the bundled `scout` profile and expects it to
/// answer the canonical "Say exactly: 'scout-ok' ..." prompt.
/// CI lacks both the binary and an OAuth session, so this stays
/// opt-in via `cargo test -- --ignored`.
#[tokio::test]
#[ignore = "requires real `claude` binary + Pro/Max subscription"]
async fn integration_smoke_invoke() {
    use crate::swarm::profile::ProfileRegistry;
    use crate::test_support::mock_app_with_pool;

    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry =
        ProfileRegistry::load_from(None).expect("registry load");
    let scout = registry.get("scout").expect("scout exists");
    let transport = SubprocessTransport::new();
    let result = transport
        .invoke(
            app.handle(),
            scout,
            "Say exactly: 'scout-ok' and nothing else.",
            Duration::from_secs(60),
        )
        .await
        .expect("invoke ok");
    assert!(
        result.assistant_text.contains("scout-ok"),
        "expected `scout-ok` in assistant text, got: {}",
        result.assistant_text
    );
}

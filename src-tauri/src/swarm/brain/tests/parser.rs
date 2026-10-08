//! Parser unit tests for [`super::super::action`] — `BrainAction`
//! deserialisation, the 4-step `parse_brain_action` extractor, and
//! `resolve_max_dispatches`. Split verbatim from the monolithic
//! `brain/tests.rs` (T3-25); the bodies below are byte-for-byte the
//! originals, dedented one level out of the legacy `mod tests {}`
//! block. These tests touch nothing but the parser, so they carry no
//! run-loop harness.

use super::super::action::MAX_DISPATCHES_ENV;
use super::super::{
    parse_brain_action, resolve_max_dispatches, BrainAction,
    DEFAULT_MAX_DISPATCHES,
};

#[test]
fn parse_dispatch_action_basic() {
    let text = r#"{"action":"dispatch","target":"agent:scout","prompt":"go","with_help_loop":true}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Dispatch {
            target,
            prompt,
            with_help_loop,
        } => {
            assert_eq!(target, "agent:scout");
            assert_eq!(prompt, "go");
            assert!(with_help_loop);
        }
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn parse_multibyte_char_straddling_scan_cap_does_not_panic() {
    // 'ü' is 2 bytes: the odd ASCII prefix forces a char to straddle
    // the 16 KiB scan cap. Must error gracefully, not panic.
    let huge = format!("x{}", "ü".repeat(32 * 1024));
    assert!(parse_brain_action(&huge).is_err());
}

#[test]
fn parse_dispatch_action_with_default_help_loop() {
    // `with_help_loop` omitted — serde default = false.
    let text = r#"{"action":"dispatch","target":"agent:planner","prompt":"plan it"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Dispatch {
            with_help_loop, ..
        } => {
            assert!(!with_help_loop);
        }
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn parse_finish_action_done() {
    let text = r#"{"action":"finish","outcome":"done","summary":"all approved"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Finish { outcome, summary } => {
            assert_eq!(outcome, "done");
            assert_eq!(summary, "all approved");
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_finish_action_failed() {
    let text = r#"{"action":"finish","outcome":"failed","summary":"reviewer rejected"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Finish { outcome, .. } => {
            assert_eq!(outcome, "failed");
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_ask_user_action() {
    let text = r#"{"action":"ask_user","question":"OAuth or API key?"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::AskUser { question } => {
            assert_eq!(question, "OAuth or API key?");
        }
        other => panic!("expected AskUser, got {other:?}"),
    }
}

#[test]
fn parse_help_outcome_action_direct_answer() {
    // body_json carries a serialised CoordinatorHelpOutcome
    // (DirectAnswer variant). Must NOT be parsed back as a
    // BrainAction itself — the brain treats body_json as opaque.
    let text = r#"{"action":"help_outcome","target":"agent:scout","body_json":"{\"action\":\"direct_answer\",\"answer\":\"use OAuth\"}"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::HelpOutcome { target, body_json } => {
            assert_eq!(target, "agent:scout");
            assert!(body_json.contains("direct_answer"));
            assert!(body_json.contains("use OAuth"));
        }
        other => panic!("expected HelpOutcome, got {other:?}"),
    }
}

#[test]
fn parse_help_outcome_action_ask_back() {
    let text = r#"{"action":"help_outcome","target":"agent:planner","body_json":"{\"action\":\"ask_back\",\"followup_question\":\"which file?\"}"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::HelpOutcome { body_json, .. } => {
            assert!(body_json.contains("ask_back"));
            assert!(body_json.contains("which file?"));
        }
        other => panic!("expected HelpOutcome, got {other:?}"),
    }
}

#[test]
fn parse_help_outcome_action_escalate() {
    let text = r#"{"action":"help_outcome","target":"agent:backend-builder","body_json":"{\"action\":\"escalate\",\"user_question\":\"OAuth or API key?\"}"}"#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::HelpOutcome { body_json, .. } => {
            assert!(body_json.contains("escalate"));
        }
        other => panic!("expected HelpOutcome, got {other:?}"),
    }
}

#[test]
fn parse_handles_fenced_json_block() {
    let text = "Some preamble.\n\n```json\n{\"action\":\"finish\",\"outcome\":\"done\",\"summary\":\"all good\"}\n```\n\nTrailing.";
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Finish { outcome, .. } => {
            assert_eq!(outcome, "done");
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_handles_first_balanced_object() {
    // Realistic LLM dump — prose around the JSON, no fence.
    let text = r#"OK. {"action":"dispatch","target":"agent:scout","prompt":"investigate"} let's go."#;
    let action = parse_brain_action(text).expect("parsed");
    match action {
        BrainAction::Dispatch { target, .. } => {
            assert_eq!(target, "agent:scout");
        }
        other => panic!("expected Dispatch, got {other:?}"),
    }
}

#[test]
fn parse_rejects_unknown_action() {
    let text = r#"{"action":"do_a_thing","target":"agent:scout"}"#;
    let err = parse_brain_action(text)
        .expect_err("unknown action rejected");
    assert_eq!(err.kind(), "swarm_invoke");
}

#[test]
fn parse_rejects_malformed_json() {
    let err = parse_brain_action("Just prose, no JSON at all.")
        .expect_err("missing JSON rejected");
    assert_eq!(err.kind(), "swarm_invoke");

    let err = parse_brain_action("not even close")
        .expect_err("garbage rejected");
    assert_eq!(err.kind(), "swarm_invoke");
}

#[test]
fn resolve_max_dispatches_default() {
    let prior = std::env::var(MAX_DISPATCHES_ENV).ok();
    std::env::remove_var(MAX_DISPATCHES_ENV);
    assert_eq!(resolve_max_dispatches(), DEFAULT_MAX_DISPATCHES);
    if let Some(v) = prior {
        std::env::set_var(MAX_DISPATCHES_ENV, v);
    }
}

//! `neuron_help` request + response parsers (WP-W4-05).
//!
//! Two parsers shared by the FSM (W4-06, deleted in W5-06) and the
//! brain (W5-03) for the specialist→Coordinator escalation contract:
//!
//! 1. **Specialist** emits a `{"neuron_help": {...}}` JSON block in
//!    its assistant_text when blocked. `parse_help_request` extracts
//!    it via the same defense-in-depth 4-step parser shape that W3-12d
//!    uses for Verdict (direct → fence-strip → first-balanced-{} →
//!    fail).
//! 2. **Coordinator** is asked what to do; replies with a structured
//!    `{"action": "direct_answer" | "ask_back" | "escalate", ...}`
//!    JSON block. `parse_coordinator_help_outcome` extracts it via
//!    the same 4-step parser.
//!
//! WP-W5-06 — the registry-level `process_help_request` +
//! `format_help_message` helpers were deleted with the FSM. The brain
//! routes help via the mailbox bus (`agent_dispatcher`'s help-loop
//! branch), which constructs its own prompt body. The pure parsers
//! here stay; both runtimes share the JSON contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;

use crate::error::AppError;

/// Specialist's structured "I'm blocked" payload. Mirrors the JSON
/// the persona emits — `reason` is a one-liner explanation,
/// `question` is what they want answered.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct HelpRequest {
    pub reason: String,
    pub question: String,
}

/// Coordinator's structured response to a help request. Three
/// outcomes covering the routing-decision space.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CoordinatorHelpOutcome {
    /// Coordinator answers directly. The `answer` string gets
    /// fed back to the specialist as a new turn ("Coordinator
    /// says: ...") so the specialist resumes with the answer in
    /// context. Status flips back to `Running`.
    DirectAnswer { answer: String },
    /// Coordinator wants more information from the specialist
    /// before answering. The `followup_question` is sent to the
    /// specialist, which replies with another turn. The FSM
    /// re-checks for `neuron_help` after that turn.
    AskBack { followup_question: String },
    /// Coordinator wants to ask the user. The `user_question`
    /// surfaces in the Orchestrator chat panel as a Clarify-shape
    /// message and the specialist's job pauses pending user input.
    Escalate { user_question: String },
}

/// Maximum bytes scanned for a help request. Defends against an
/// adversarial assistant_text that's mostly garbage with a tiny
/// JSON block hidden in the middle — bounded scan keeps the parser
/// O(1) per turn instead of O(text length).
const HELP_REQUEST_SCAN_CAP: usize = 16 * 1024;

/// Parse a `neuron_help` JSON block from a specialist's
/// `assistant_text`. Returns `None` if no block is present (the
/// most common case — specialists are usually unblocked).
///
/// Defense-in-depth: tries 4 strategies in order, falling through
/// on parse failure:
///   1. Whole text is JSON
///   2. JSON inside ```json ... ``` fence
///   3. First balanced `{...}` substring scanned from start
///   4. Bail (return None)
///
/// The `neuron_help` key is the marker — only blocks that have it
/// at the top level are considered. Bare JSON (no `neuron_help`
/// key) is not a help request.
pub fn parse_help_request(assistant_text: &str) -> Option<HelpRequest> {
    let truncated = if assistant_text.len() > HELP_REQUEST_SCAN_CAP {
        &assistant_text[..HELP_REQUEST_SCAN_CAP]
    } else {
        assistant_text
    };

    // 1. Whole-text JSON.
    if let Some(req) = try_extract_help_request(truncated.trim()) {
        return Some(req);
    }
    // 2. ```json fence strip.
    if let Some(fenced) = strip_fence(truncated) {
        if let Some(req) = try_extract_help_request(fenced.trim()) {
            return Some(req);
        }
    }
    // 3. First balanced {...}.
    if let Some(balanced) = first_balanced_object(truncated) {
        if let Some(req) = try_extract_help_request(balanced) {
            return Some(req);
        }
    }
    // 4. Bail.
    None
}

/// Parse a `CoordinatorHelpOutcome` from the Coordinator's
/// assistant_text reply. Same 4-step strategy as
/// `parse_help_request`. Returns `Err(SwarmInvoke)` when no valid
/// outcome JSON is present — unlike the request parser, the
/// coordinator IS expected to respond in shape, so missing JSON is
/// a hard error the FSM can surface.
pub fn parse_coordinator_help_outcome(
    assistant_text: &str,
) -> Result<CoordinatorHelpOutcome, AppError> {
    let truncated = if assistant_text.len() > HELP_REQUEST_SCAN_CAP {
        &assistant_text[..HELP_REQUEST_SCAN_CAP]
    } else {
        assistant_text
    };
    if let Some(out) = try_parse_outcome(truncated.trim()) {
        return Ok(out);
    }
    if let Some(fenced) = strip_fence(truncated) {
        if let Some(out) = try_parse_outcome(fenced.trim()) {
            return Ok(out);
        }
    }
    if let Some(balanced) = first_balanced_object(truncated) {
        if let Some(out) = try_parse_outcome(balanced) {
            return Ok(out);
        }
    }
    Err(AppError::SwarmInvoke(format!(
        "coordinator help outcome JSON not found in reply (first 200 chars: {})",
        truncated.chars().take(200).collect::<String>()
    )))
}

/// Helper: inner parse step that interprets a candidate JSON
/// fragment as either a `{neuron_help: {...}}` wrapper or a bare
/// `{reason, question}` shape. Both shapes are accepted so a
/// persona that drops the wrapper still works.
fn try_extract_help_request(s: &str) -> Option<HelpRequest> {
    let v: Value = serde_json::from_str(s).ok()?;
    // Wrapper shape.
    if let Some(inner) = v.get("neuron_help") {
        return serde_json::from_value::<HelpRequest>(inner.clone()).ok();
    }
    // Bare shape — defensive; not the documented contract but
    // common LLM divergence.
    serde_json::from_value::<HelpRequest>(v).ok()
}

/// Helper: inner parse for `CoordinatorHelpOutcome`.
fn try_parse_outcome(s: &str) -> Option<CoordinatorHelpOutcome> {
    serde_json::from_str::<CoordinatorHelpOutcome>(s).ok()
}

/// Strip the FIRST ```json ... ``` (or ```...```) fence in `s` and
/// return the inner contents. Returns None when no fence is
/// present.
fn strip_fence(s: &str) -> Option<&str> {
    let start_idx = s.find("```")?;
    let after_open = &s[start_idx + 3..];
    let after_lang = match after_open.find('\n') {
        Some(n) => &after_open[n + 1..],
        None => after_open,
    };
    let close_idx = after_lang.find("```")?;
    Some(&after_lang[..close_idx])
}

/// Walk `s` and return the FIRST balanced `{...}` substring (count
/// braces, account for strings). Returns None when no balanced
/// object is found.
fn first_balanced_object(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut start: Option<usize> = None;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s_idx) = start {
                        return std::str::from_utf8(&bytes[s_idx..=i]).ok();
                    }
                }
            }
            _ => {}
        }
    }
    None
}

// WP-W5-06 — `process_help_request` and `format_help_message`
// were the registry-level helpers the FSM (`RegistryTransport`)
// invoked when a specialist emitted `neuron_help`. With the FSM
// gone, the brain (W5-03) routes help via the mailbox bus
// + `agent_dispatcher::handle_help_request_via_mailbox`. The
// parsers above (`parse_help_request`,
// `parse_coordinator_help_outcome`) stay — both runtimes share
// the same JSON contract.

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_help_request --

    #[test]
    fn parses_wrapper_shape_direct_json() {
        let text = r#"{"neuron_help": {"reason": "missing spec", "question": "which file?"}}"#;
        let req = parse_help_request(text).expect("parsed");
        assert_eq!(req.reason, "missing spec");
        assert_eq!(req.question, "which file?");
    }

    #[test]
    fn parses_wrapper_inside_fence() {
        let text =
            "Some preamble.\n\n```json\n{\"neuron_help\": {\"reason\": \"r\", \"question\": \"q\"}}\n```\n\nTrailing.";
        let req = parse_help_request(text).expect("parsed");
        assert_eq!(req.reason, "r");
        assert_eq!(req.question, "q");
    }

    #[test]
    fn parses_wrapper_inline_balanced_object() {
        // Realistic LLM dump — prose around the JSON, no fence.
        let text =
            r#"I think I'm stuck. Here's a help request: {"neuron_help": {"reason": "auth flow ambiguous", "question": "should we use OAuth or API key?"}} please advise."#;
        let req = parse_help_request(text).expect("parsed");
        assert_eq!(req.reason, "auth flow ambiguous");
    }

    #[test]
    fn parses_bare_shape_without_wrapper() {
        let text = r#"{"reason": "no wrapper", "question": "still works"}"#;
        let req = parse_help_request(text).expect("parsed");
        assert_eq!(req.reason, "no wrapper");
    }

    #[test]
    fn returns_none_when_no_help_block_present() {
        assert!(parse_help_request("Just regular assistant output.").is_none());
        assert!(parse_help_request("").is_none());
        assert!(parse_help_request("{}").is_none());
    }

    #[test]
    fn returns_none_for_unrelated_json() {
        let text = r#"{"completely": "different"}"#;
        assert!(parse_help_request(text).is_none());
    }

    #[test]
    fn truncated_long_input_does_not_panic() {
        let huge = "x".repeat(50 * 1024);
        // Just ensure we don't panic on > scan cap. Result irrelevant.
        let _ = parse_help_request(&huge);
    }

    // -- parse_coordinator_help_outcome --

    #[test]
    fn parses_direct_answer() {
        let text = r#"{"action": "direct_answer", "answer": "use OAuth"}"#;
        let outcome = parse_coordinator_help_outcome(text).expect("parsed");
        match outcome {
            CoordinatorHelpOutcome::DirectAnswer { answer } => {
                assert_eq!(answer, "use OAuth");
            }
            other => panic!("expected DirectAnswer, got {other:?}"),
        }
    }

    #[test]
    fn parses_ask_back_with_fence() {
        let text =
            "```json\n{\"action\": \"ask_back\", \"followup_question\": \"what's X?\"}\n```";
        let outcome = parse_coordinator_help_outcome(text).expect("parsed");
        match outcome {
            CoordinatorHelpOutcome::AskBack { followup_question } => {
                assert_eq!(followup_question, "what's X?");
            }
            other => panic!("expected AskBack, got {other:?}"),
        }
    }

    #[test]
    fn parses_escalate_inline() {
        let text =
            r#"OK. {"action": "escalate", "user_question": "OAuth or API key?"} done."#;
        let outcome = parse_coordinator_help_outcome(text).expect("parsed");
        match outcome {
            CoordinatorHelpOutcome::Escalate { user_question } => {
                assert_eq!(user_question, "OAuth or API key?");
            }
            other => panic!("expected Escalate, got {other:?}"),
        }
    }

    #[test]
    fn missing_outcome_json_returns_swarminvoke() {
        let text = "Just prose, no JSON at all.";
        let err = parse_coordinator_help_outcome(text)
            .expect_err("missing JSON should error");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    #[test]
    fn unknown_action_returns_swarminvoke() {
        let text = r#"{"action": "smell_check", "answer": "..."}"#;
        let err = parse_coordinator_help_outcome(text)
            .expect_err("unknown action rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    // -- helpers --

    #[test]
    fn strip_fence_extracts_inner_content() {
        let s = "before\n```json\n{\"x\":1}\n```\nafter";
        assert_eq!(strip_fence(s), Some("{\"x\":1}\n"));
    }

    #[test]
    fn strip_fence_returns_none_without_fence() {
        assert!(strip_fence("no fences here").is_none());
    }

    #[test]
    fn first_balanced_object_handles_strings_with_braces() {
        let s = r#"hi {"key": "value with } inside", "n": 1} bye"#;
        assert_eq!(
            first_balanced_object(s),
            Some(r#"{"key": "value with } inside", "n": 1}"#)
        );
    }
}

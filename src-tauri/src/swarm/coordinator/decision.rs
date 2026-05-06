//! Coordinator brain decision schema + robust JSON parser
//! (WP-W3-12f §2).
//!
//! The Coordinator specialist (the 6th bundled profile) returns its
//! routing decision as a JSON object matching the
//! `CoordinatorDecision` shape below. As with the W3-12d Verdict
//! pipeline, real LLMs occasionally wrap their output in markdown
//! fences or prepend a conversational preamble despite the strict
//! OUTPUT CONTRACT in the persona body, so the parser walks four
//! progressively more lenient strategies before giving up. See
//! architectural report §7.1 ("Robust JSON Extraction") for the
//! four-step recipe.
//!
//! The Verdict and Decision parsers run the same brace-counting +
//! fence-stripping logic; they were intentionally duplicated rather
//! than consolidated into a generic `parse_robust_json<T>` helper.
//! See the module-level note next to `parse_decision` for the
//! rationale.
//!
//! Cross-runtime hygiene: this module imports only from `serde_json`
//! and `crate::error::AppError`. No `sidecar/`, no `agent_runtime/`,
//! no Tauri runtime — the parser is a pure helper the FSM calls
//! after `transport.invoke` returns the assistant text.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Routing rail the Coordinator picks for the job. Wire form is
/// snake_case (`"research_only"` / `"execute_plan"`) so the
/// frontend bindings match the persona OUTPUT CONTRACT verbatim.
///
/// - `ResearchOnly` — short-circuit the FSM after Classify; Scout's
///   findings are the deliverable. Used for "explain X / what does
///   Y do" style goals where the full 5-stage chain would burn cost
///   producing empty Plan/Build outputs.
/// - `ExecutePlan` — fall through to the canonical Plan / Build /
///   Review / Test chain. The default-fail-open target when the
///   Coordinator output is unparseable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorRoute {
    ResearchOnly,
    ExecutePlan,
}

/// Single-shot routing decision the Coordinator emits after Scout.
/// Stamped onto the Classify stage's `StageResult.coordinator_decision`
/// and surfaced to the UI via `SwarmJobEvent::DecisionMade`.
///
/// `reasoning` is a one-sentence rationale per the OUTPUT CONTRACT;
/// the FSM treats it as informational only — the routing branch
/// keys off `route` alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CoordinatorDecision {
    pub route: CoordinatorRoute,
    pub reasoning: String,
}

/// Parse a `CoordinatorDecision` from arbitrary assistant text.
/// Tries four strategies in order:
///
/// 1. Direct `serde_json::from_str` on the trimmed input.
/// 2. Strip a leading / trailing markdown fence (` ```json ... ``` `
///    or unlabelled ` ``` ... ``` `) and try again.
/// 3. Find the first balanced `{...}` substring (string-aware so
///    `{"reasoning":"a } b"}` is detected as one block) and try that.
/// 4. Fail with `AppError::SwarmInvoke`, including the first 400
///    *characters* of the raw input for diagnostics.
///
/// All four steps use `serde_json` so missing fields, wrong-typed
/// fields, or unknown enum variants surface as parse errors at
/// step 1 already (we don't silently coerce). The fence + balanced-
/// substring steps are pure string surgery; they never touch the
/// validation pipeline.
///
/// **Note on duplication with `parse_verdict`.** The two parsers
/// share identical structure; the WP suggested an optional refactor
/// to a generic `parse_robust_json<T>` helper. We chose duplication
/// because: (a) the call sites' diagnostics differ ("could not
/// parse Verdict" vs "could not parse CoordinatorDecision") so a
/// generic helper would need a `type_name` parameter that's
/// awkward; (b) the two shapes are semantically distinct enough
/// (Verdict gates retry; Decision short-circuits the chain) that
/// keeping their parsers independent makes future divergence
/// (e.g. Decision needing to accept a string-only "research_only"
/// fallback) a one-file edit. See `verdict::parse_verdict` for the
/// sibling implementation.
pub fn parse_decision(raw: &str) -> Result<CoordinatorDecision, AppError> {
    // Step 1: direct parse — covers the happy path where the
    // persona obeyed the OUTPUT CONTRACT to the letter.
    let trimmed = raw.trim();
    if let Ok(d) = serde_json::from_str::<CoordinatorDecision>(trimmed) {
        return Ok(d);
    }
    // Step 2: strip a markdown fence wrapping. Common pattern when
    // the LLM falls back to "render as markdown code block" reflex.
    if let Some(stripped) = strip_markdown_fence(raw) {
        if let Ok(d) = serde_json::from_str::<CoordinatorDecision>(stripped) {
            return Ok(d);
        }
    }
    // Step 3: scan for the first balanced `{...}` substring. Covers
    // "Here's my decision: { ... }" and similar conversational
    // preambles. String-aware brace counting so quoted `}` doesn't
    // close the object early.
    if let Some(sub) = first_balanced_json_object(raw) {
        if let Ok(d) = serde_json::from_str::<CoordinatorDecision>(sub) {
            return Ok(d);
        }
    }
    // Step 4: give up. The 400-char preview is char-bounded (NOT
    // byte-bounded) so multi-byte Turkish text is never split
    // mid-codepoint in the error message.
    Err(AppError::SwarmInvoke(format!(
        "could not parse CoordinatorDecision from assistant text: {}",
        truncate_chars(raw, 400)
    )))
}

/// Strip a single markdown code fence wrapping. Returns the inner
/// text (without the fence lines) when the input is a fenced block,
/// else `None`. Recognises:
///
/// - ` ```json\n ... \n``` ` (language-tagged, the canonical form)
/// - ` ```\n ... \n``` ` (untagged)
/// - Trailing newline / whitespace before / after the fences is
///   tolerated so the fence-stripping step is idempotent.
fn strip_markdown_fence(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let after_open = trimmed.strip_prefix("```")?;
    let after_lang = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => return None,
    };
    let close_idx = after_lang.rfind("```")?;
    let inner = &after_lang[..close_idx];
    Some(inner.trim_end_matches('\n').trim())
}

/// Find the first balanced `{...}` substring in `raw`. String-aware:
/// a `{` or `}` inside a `"..."` literal does not affect the depth
/// counter, and a backslash-escaped `\"` inside that literal does
/// not close the string.
///
/// Returns `None` if there is no `{` at all, or if the input is
/// unbalanced (more `{` than `}` even at end-of-input).
fn first_balanced_json_object(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut prev_was_backslash = false;
    for (idx, ch) in raw[start..].char_indices() {
        let abs = start + idx;
        if in_string {
            if prev_was_backslash {
                prev_was_backslash = false;
                continue;
            }
            match ch {
                '\\' => prev_was_backslash = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let end = abs + ch.len_utf8();
                    return Some(&raw[start..end]);
                }
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Truncate `s` to at most `max_chars` Unicode characters. Bounded
/// by `chars()` (not bytes) so multi-byte Turkish text is never
/// split mid-codepoint in the error message.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    fn execute_fixture() -> CoordinatorDecision {
        CoordinatorDecision {
            route: CoordinatorRoute::ExecutePlan,
            reasoning: "test fixture".to_string(),
        }
    }

    /// Step 1 path — bare JSON object, no fence, no preamble.
    #[test]
    fn parse_decision_direct_object() {
        let raw = r#"{"route":"execute_plan","reasoning":"test fixture"}"#;
        let d = parse_decision(raw).expect("direct parse");
        assert_eq!(d, execute_fixture());
    }

    /// Step 1 path — research_only variant round-trips.
    #[test]
    fn parse_decision_research_only_variant() {
        let raw = r#"{"route":"research_only","reasoning":"explain only"}"#;
        let d = parse_decision(raw).expect("research_only parse");
        assert_eq!(d.route, CoordinatorRoute::ResearchOnly);
        assert_eq!(d.reasoning, "explain only");
    }

    /// Step 2 path — language-tagged markdown fence.
    #[test]
    fn parse_decision_with_json_fence() {
        let raw = "```json\n{\"route\":\"execute_plan\",\"reasoning\":\"test fixture\"}\n```";
        let d = parse_decision(raw).expect("fenced parse");
        assert_eq!(d, execute_fixture());
    }

    /// Step 2 path — unlabelled markdown fence.
    #[test]
    fn parse_decision_with_unlabeled_fence() {
        let raw = "```\n{\"route\":\"research_only\",\"reasoning\":\"r\"}\n```";
        let d = parse_decision(raw).expect("unlabelled fenced parse");
        assert_eq!(d.route, CoordinatorRoute::ResearchOnly);
    }

    /// Step 3 path — preamble before the JSON. Balanced-substring
    /// scan recovers the decision.
    #[test]
    fn parse_decision_with_preamble_and_json() {
        let raw = "Here's my decision:\n{\"route\":\"execute_plan\",\"reasoning\":\"test fixture\"}";
        let d = parse_decision(raw).expect("preamble parse");
        assert_eq!(d, execute_fixture());
    }

    /// Garbage in → `AppError::SwarmInvoke` out, with the input
    /// preview embedded for diagnostics.
    #[test]
    fn parse_decision_unparseable_returns_error() {
        let err = parse_decision("lol idk").expect_err("garbage rejected");
        assert_eq!(err.kind(), "swarm_invoke");
        assert!(
            err.message().contains("could not parse CoordinatorDecision"),
            "error should mention parse failure: {}",
            err.message()
        );
    }

    /// Both `CoordinatorRoute` variants serialize as snake_case on
    /// the wire — guards against future renames silently breaking the
    /// frontend bindings or the persona OUTPUT CONTRACT.
    #[test]
    fn coordinator_route_serializes_snake_case() {
        let r1 = serde_json::to_string(&CoordinatorRoute::ResearchOnly)
            .expect("ser research_only");
        assert_eq!(r1, "\"research_only\"");
        let r2 = serde_json::to_string(&CoordinatorRoute::ExecutePlan)
            .expect("ser execute_plan");
        assert_eq!(r2, "\"execute_plan\"");
        // Round-trip both variants through the parser as well.
        let d1 = parse_decision(r#"{"route":"research_only","reasoning":"x"}"#)
            .expect("rt research_only");
        assert_eq!(d1.route, CoordinatorRoute::ResearchOnly);
        let d2 = parse_decision(r#"{"route":"execute_plan","reasoning":"x"}"#)
            .expect("rt execute_plan");
        assert_eq!(d2.route, CoordinatorRoute::ExecutePlan);
    }

    /// Brace-counting must skip braces inside string literals so a
    /// `{"reasoning":"a } b"}` with a stray `}` in the reasoning
    /// still parses via the balanced-substring path.
    #[test]
    fn parse_decision_balanced_braces_with_strings() {
        let raw = r#"OK here it is: {"route":"execute_plan","reasoning":"a } b { c"}"#;
        let d = parse_decision(raw).expect("braced reasoning parse");
        assert_eq!(d.route, CoordinatorRoute::ExecutePlan);
        assert_eq!(d.reasoning, "a } b { c");
    }

    /// Unicode (Turkish + emoji) in the reasoning survives all four
    /// steps — including the truncation logic in the error path.
    #[test]
    fn parse_decision_unicode_safe() {
        let raw = r#"{"route":"research_only","reasoning":"İşler yolunda 🚀"}"#;
        let d = parse_decision(raw).expect("unicode parse");
        assert_eq!(d.reasoning, "İşler yolunda 🚀");

        // Force the error-path through unicode so the 400-char
        // truncation never splits on a multi-byte boundary.
        let garbage = "çş".repeat(500) + " not actually json";
        let err = parse_decision(&garbage).expect_err("garbage rejected");
        assert!(err.message().contains("could not parse CoordinatorDecision"));
    }

    /// Unknown route variant (typo) surfaces as a parse error rather
    /// than silently coercing to a default. The frontend bindings
    /// rely on this strictness.
    #[test]
    fn parse_decision_unknown_route_rejected() {
        let raw = r#"{"route":"yolo","reasoning":"x"}"#;
        let err = parse_decision(raw).expect_err("unknown route rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }
}

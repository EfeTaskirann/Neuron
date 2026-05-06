//! Orchestrator outcome schema + robust JSON parser (WP-W3-12k1 §2).
//!
//! The Orchestrator (the 9th bundled profile) sits **above** the
//! Coordinator FSM. Each call to `swarm:orchestrator_decide` spawns a
//! one-shot `claude` subprocess against the `orchestrator.md` persona
//! and returns an `OrchestratorOutcome` — a single routing decision
//! per user message: `DirectReply` (short conversational answer),
//! `Clarify` (return a follow-up question), or `Dispatch` (return a
//! refined goal the frontend can feed into `swarm:run_job`).
//!
//! The parser walks four progressively more lenient strategies before
//! giving up, matching the recipe from architectural report §7.1
//! ("Robust JSON Extraction") and the existing
//! `verdict::parse_verdict` / `decision::parse_decision` siblings.
//!
//! **Note on duplication.** The four-step recipe is duplicated rather
//! than extracted into a generic `parse_robust_json<T>` helper, per
//! W3-12f's documented choice (see the same note in `decision.rs`):
//! the call sites' diagnostics differ ("could not parse Verdict" vs
//! "could not parse CoordinatorDecision" vs "could not parse
//! OrchestratorOutcome") and the three shapes are semantically
//! distinct enough that keeping their parsers independent makes
//! future divergence (e.g. an Orchestrator-only fallback to a default
//! `DirectReply` when raw text is unparseable) a one-file edit.
//!
//! Cross-runtime hygiene: this module imports only from `serde`,
//! `serde_json`, `specta`, and `crate::error::AppError`. No
//! `sidecar/`, no `agent_runtime/`, no Tauri runtime — the parser is
//! a pure helper the IPC handler calls after `transport.invoke`
//! returns the assistant text.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Three-way routing decision the Orchestrator emits per user
/// message. Wire form is snake_case
/// (`"direct_reply"` / `"clarify"` / `"dispatch"`) so the frontend
/// bindings match the persona OUTPUT CONTRACT verbatim.
///
/// - `DirectReply` — the assistant answers the user directly. The
///   `OrchestratorOutcome.text` carries the answer.
/// - `Clarify` — the user's message is too ambiguous to dispatch.
///   The `OrchestratorOutcome.text` carries a clarifying question
///   to show back to the user.
/// - `Dispatch` — the user's message is concrete enough to feed
///   `swarm:run_job`. The `OrchestratorOutcome.text` carries the
///   refined goal the frontend will pass to the Coordinator FSM.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorAction {
    DirectReply,
    Clarify,
    Dispatch,
}

/// Single-shot Orchestrator decision. `text` carries the active
/// payload depending on `action`:
///
/// - `DirectReply`: assistant's answer to show the user.
/// - `Clarify`: clarifying question to show the user.
/// - `Dispatch`: refined goal the frontend feeds into
///   `swarm:run_job`.
///
/// `reasoning` is a one-sentence rationale per the OUTPUT CONTRACT;
/// it's informational only — the frontend branches off `action`.
///
/// **Stateless** per W3-12k1 contract: this struct is the entire
/// per-call return surface. No persisted history, no thread id, no
/// turn count. W3-12k-2 layers persistent context on top.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct OrchestratorOutcome {
    pub action: OrchestratorAction,
    pub text: String,
    pub reasoning: String,
}

/// Parse an `OrchestratorOutcome` from arbitrary assistant text.
/// Tries four strategies in order:
///
/// 1. Direct `serde_json::from_str` on the trimmed input.
/// 2. Strip a leading / trailing markdown fence (` ```json ... ``` `
///    or unlabelled ` ``` ... ``` `) and try again.
/// 3. Find the first balanced `{...}` substring (string-aware so
///    `{"text":"a } b"}` is detected as one block) and try that.
/// 4. Fail with `AppError::SwarmInvoke`, including the first 400
///    *characters* of the raw input for diagnostics.
///
/// All four steps use `serde_json` so missing fields, wrong-typed
/// fields, or unknown enum variants surface as parse errors at
/// step 1 already (we don't silently coerce). The fence + balanced-
/// substring steps are pure string surgery; they never touch the
/// validation pipeline.
///
/// See the module-level note for the rationale on duplicating this
/// recipe across `verdict.rs`, `decision.rs`, and `orchestrator.rs`.
pub fn parse_orchestrator_outcome(
    raw: &str,
) -> Result<OrchestratorOutcome, AppError> {
    // Step 1: direct parse — covers the happy path where the
    // persona obeyed the OUTPUT CONTRACT to the letter.
    let trimmed = raw.trim();
    if let Ok(o) = serde_json::from_str::<OrchestratorOutcome>(trimmed) {
        return Ok(o);
    }
    // Step 2: strip a markdown fence wrapping. Common pattern when
    // the LLM falls back to "render as markdown code block" reflex.
    if let Some(stripped) = strip_markdown_fence(raw) {
        if let Ok(o) = serde_json::from_str::<OrchestratorOutcome>(stripped) {
            return Ok(o);
        }
    }
    // Step 3: scan for the first balanced `{...}` substring. Covers
    // "Here's my decision: { ... }" and similar conversational
    // preambles. String-aware brace counting so quoted `}` doesn't
    // close the object early.
    if let Some(sub) = first_balanced_json_object(raw) {
        if let Ok(o) = serde_json::from_str::<OrchestratorOutcome>(sub) {
            return Ok(o);
        }
    }
    // Step 4: give up. The 400-char preview is char-bounded (NOT
    // byte-bounded) so multi-byte Turkish text is never split
    // mid-codepoint in the error message.
    Err(AppError::SwarmInvoke(format!(
        "could not parse OrchestratorOutcome from assistant text: {}",
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
/// unbalanced (more `}` than `{` at any point).
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

    /// Step 1 path — direct_reply variant, bare JSON object.
    #[test]
    fn parse_orchestrator_outcome_direct_reply_variant() {
        let raw =
            r#"{"action":"direct_reply","text":"merhaba!","reasoning":"selamlama"}"#;
        let o = parse_orchestrator_outcome(raw).expect("direct_reply parse");
        assert_eq!(o.action, OrchestratorAction::DirectReply);
        assert_eq!(o.text, "merhaba!");
        assert_eq!(o.reasoning, "selamlama");
    }

    /// Step 1 path — clarify variant.
    #[test]
    fn parse_orchestrator_outcome_clarify_variant() {
        let raw = r#"{"action":"clarify","text":"hangi dosya?","reasoning":"yol eksik"}"#;
        let o = parse_orchestrator_outcome(raw).expect("clarify parse");
        assert_eq!(o.action, OrchestratorAction::Clarify);
        assert_eq!(o.text, "hangi dosya?");
        assert_eq!(o.reasoning, "yol eksik");
    }

    /// Step 1 path — dispatch variant.
    #[test]
    fn parse_orchestrator_outcome_dispatch_variant() {
        let raw = r#"{"action":"dispatch","text":"EXECUTE: Add doc to X.tsx","reasoning":"somut iş"}"#;
        let o = parse_orchestrator_outcome(raw).expect("dispatch parse");
        assert_eq!(o.action, OrchestratorAction::Dispatch);
        assert_eq!(o.text, "EXECUTE: Add doc to X.tsx");
        assert_eq!(o.reasoning, "somut iş");
    }

    /// Step 2 path — language-tagged markdown fence.
    #[test]
    fn parse_orchestrator_outcome_with_json_fence() {
        let raw = "```json\n{\"action\":\"direct_reply\",\"text\":\"hi\",\"reasoning\":\"r\"}\n```";
        let o = parse_orchestrator_outcome(raw).expect("fenced parse");
        assert_eq!(o.action, OrchestratorAction::DirectReply);
        assert_eq!(o.text, "hi");
    }

    /// Step 2 path — unlabelled markdown fence.
    #[test]
    fn parse_orchestrator_outcome_with_unlabeled_fence() {
        let raw = "```\n{\"action\":\"clarify\",\"text\":\"q?\",\"reasoning\":\"r\"}\n```";
        let o = parse_orchestrator_outcome(raw).expect("unlabelled fence parse");
        assert_eq!(o.action, OrchestratorAction::Clarify);
    }

    /// Step 3 path — preamble before the JSON. Balanced-substring
    /// scan recovers the outcome.
    #[test]
    fn parse_orchestrator_outcome_with_preamble() {
        let raw = "Here's my decision:\n{\"action\":\"dispatch\",\"text\":\"EXECUTE: foo\",\"reasoning\":\"concrete\"}";
        let o = parse_orchestrator_outcome(raw).expect("preamble parse");
        assert_eq!(o.action, OrchestratorAction::Dispatch);
        assert_eq!(o.text, "EXECUTE: foo");
    }

    /// Garbage in → `AppError::SwarmInvoke` out, with the input
    /// preview embedded for diagnostics.
    #[test]
    fn parse_orchestrator_outcome_unparseable_returns_error() {
        let err = parse_orchestrator_outcome("lol idk")
            .expect_err("garbage rejected");
        assert_eq!(err.kind(), "swarm_invoke");
        assert!(
            err.message().contains("could not parse OrchestratorOutcome"),
            "error should mention parse failure: {}",
            err.message()
        );
    }

    /// Unknown action variant (typo) surfaces as a parse error
    /// rather than silently coercing — symmetric with the strictness
    /// in `parse_decision_unknown_route_rejected`.
    #[test]
    fn parse_orchestrator_outcome_unknown_action_rejected() {
        let raw = r#"{"action":"do_x","text":"...","reasoning":"..."}"#;
        let err = parse_orchestrator_outcome(raw)
            .expect_err("unknown action rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    /// All three `OrchestratorAction` variants serialize as
    /// snake_case on the wire — guards the frontend bindings + the
    /// persona OUTPUT CONTRACT against silent renames.
    #[test]
    fn orchestrator_action_serializes_snake_case() {
        let s1 = serde_json::to_string(&OrchestratorAction::DirectReply)
            .expect("ser direct_reply");
        assert_eq!(s1, "\"direct_reply\"");
        let s2 = serde_json::to_string(&OrchestratorAction::Clarify)
            .expect("ser clarify");
        assert_eq!(s2, "\"clarify\"");
        let s3 = serde_json::to_string(&OrchestratorAction::Dispatch)
            .expect("ser dispatch");
        assert_eq!(s3, "\"dispatch\"");
        // Round-trip each variant through the parser.
        let o1 = parse_orchestrator_outcome(
            r#"{"action":"direct_reply","text":"x","reasoning":"y"}"#,
        )
        .expect("rt direct_reply");
        assert_eq!(o1.action, OrchestratorAction::DirectReply);
        let o2 = parse_orchestrator_outcome(
            r#"{"action":"clarify","text":"x","reasoning":"y"}"#,
        )
        .expect("rt clarify");
        assert_eq!(o2.action, OrchestratorAction::Clarify);
        let o3 = parse_orchestrator_outcome(
            r#"{"action":"dispatch","text":"x","reasoning":"y"}"#,
        )
        .expect("rt dispatch");
        assert_eq!(o3.action, OrchestratorAction::Dispatch);
    }

    /// Brace-counting must skip braces inside string literals so a
    /// `{"text":"a } b"}` with a stray `}` in the text still parses
    /// via the balanced-substring path.
    #[test]
    fn parse_orchestrator_outcome_balanced_braces_with_strings() {
        let raw = r#"OK: {"action":"clarify","text":"a } b { c","reasoning":"r"}"#;
        let o = parse_orchestrator_outcome(raw).expect("braced text parse");
        assert_eq!(o.action, OrchestratorAction::Clarify);
        assert_eq!(o.text, "a } b { c");
    }

    /// Unicode (Turkish + emoji) in the text survives all four
    /// steps — including the truncation logic in the error path.
    #[test]
    fn parse_orchestrator_outcome_unicode_safe() {
        let raw = r#"{"action":"direct_reply","text":"Selam 🚀","reasoning":"İşler yolunda"}"#;
        let o = parse_orchestrator_outcome(raw).expect("unicode parse");
        assert_eq!(o.text, "Selam 🚀");
        assert_eq!(o.reasoning, "İşler yolunda");

        // Force the error-path through unicode so the 400-char
        // truncation never splits on a multi-byte boundary.
        let garbage = "çş".repeat(500) + " not actually json";
        let err = parse_orchestrator_outcome(&garbage)
            .expect_err("garbage rejected");
        assert!(err
            .message()
            .contains("could not parse OrchestratorOutcome"));
    }
}

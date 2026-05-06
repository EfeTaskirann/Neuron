//! Verdict schema + robust JSON parser (WP-W3-12d §2 + §3).
//!
//! The Reviewer + IntegrationTester specialists return their decision
//! as a JSON object matching the `Verdict` shape below. Real LLMs
//! occasionally wrap their output in markdown fences or prepend a
//! conversational preamble despite the strict OUTPUT CONTRACT in the
//! persona body, so the parser walks four progressively more lenient
//! strategies before giving up. See architectural report §7.1
//! ("Robust JSON Extraction") for the four-step recipe.
//!
//! Cross-runtime hygiene: this module imports only from `serde_json`
//! and `crate::error::AppError`. No `sidecar/`, no `agent_runtime/`,
//! no Tauri runtime — the parser is a pure helper the FSM calls
//! after `transport.invoke` returns the assistant text.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Severity of a single Verdict issue. Reviewers grade findings on
/// this three-rung ladder; Tester surfaces failing-test names with
/// `High` for hard failures and `Med` for flakes-suspected.
///
/// Wire form is snake_case (`"high"` / `"med"` / `"low"`) so the
/// frontend bindings match the persona contract verbatim.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "snake_case")]
pub enum VerdictSeverity {
    High,
    Med,
    Low,
}

/// One Reviewer / Tester finding. `file` + `line` are optional so a
/// summary-only verdict (e.g. "tests passed, nothing to nit") can
/// emit an empty issues list and still be valid.
///
/// `message` is renamed `msg` on the wire to match the persona
/// OUTPUT CONTRACT — keeping the JSON shape concise reduces the
/// odds of LLMs wandering off-shape mid-stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct VerdictIssue {
    pub severity: VerdictSeverity,
    pub file: Option<String>,
    pub line: Option<u32>,
    #[serde(rename = "msg")]
    pub message: String,
}

/// The Reviewer / Tester output. `approved=true` means "advance to
/// the next stage"; `approved=false` finalizes the job as Failed
/// and the issue list lands in the persisted `last_verdict_json`.
///
/// Per WP §"Out of scope" there is NO retry loop in W3-12d — a
/// rejected verdict is terminal. W3-12e adds the feedback loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub approved: bool,
    pub issues: Vec<VerdictIssue>,
    pub summary: String,
}

impl Verdict {
    /// Inverse of `approved`. Pulled out as a named method so call
    /// sites read as `if verdict.rejected() { … }` instead of
    /// `!verdict.approved`.
    pub fn rejected(&self) -> bool {
        !self.approved
    }
}

/// Parse a `Verdict` from arbitrary assistant text. Tries four
/// strategies in order:
///
/// 1. Direct `serde_json::from_str` on the trimmed input.
/// 2. Strip a leading / trailing markdown fence (` ```json ... ``` `
///    or unlabelled ` ``` ... ``` `) and try again.
/// 3. Find the first balanced `{...}` substring (string-aware so
///    `{"summary":"a } b"}` is detected as one block) and try that.
/// 4. Fail with `AppError::SwarmInvoke`, including the first 400
///    *characters* of the raw input for diagnostics.
///
/// All four steps use `serde_json` so missing fields, wrong-typed
/// fields, or unknown enum variants surface as parse errors at
/// step 1 already (we don't silently coerce). The fence + balanced-
/// substring steps are pure string surgery; they never touch the
/// validation pipeline.
pub fn parse_verdict(raw: &str) -> Result<Verdict, AppError> {
    // Step 1: direct parse — covers the happy path where the
    // persona obeyed the OUTPUT CONTRACT to the letter.
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Verdict>(trimmed) {
        return Ok(v);
    }
    // Step 2: strip a markdown fence wrapping. Common pattern when
    // the LLM falls back to "render as markdown code block" reflex.
    if let Some(stripped) = strip_markdown_fence(raw) {
        if let Ok(v) = serde_json::from_str::<Verdict>(stripped) {
            return Ok(v);
        }
    }
    // Step 3: scan for the first balanced `{...}` substring. Covers
    // "Here's my verdict: { ... }" and similar conversational
    // preambles. String-aware brace counting so quoted `}` doesn't
    // close the object early.
    if let Some(sub) = first_balanced_json_object(raw) {
        if let Ok(v) = serde_json::from_str::<Verdict>(sub) {
            return Ok(v);
        }
    }
    // Step 4: give up. The 400-char preview is char-bounded (NOT
    // byte-bounded) so multi-byte Turkish text is never split
    // mid-codepoint in the error message.
    Err(AppError::SwarmInvoke(format!(
        "could not parse Verdict from assistant text: {}",
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
///
/// We do NOT strip multiple nested fences — if a verdict contains a
/// fenced block in its `summary` text the inner fence is part of the
/// JSON string literal and serde_json handles it. Only the outer
/// wrapper is in scope here.
fn strip_markdown_fence(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    // Must start with three backticks. After the backticks we may
    // see an optional language tag (any non-newline run) followed
    // by a newline.
    let after_open = trimmed.strip_prefix("```")?;
    let after_lang = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => return None,
    };
    // Find the closing fence. We scan from the end so a verdict that
    // happens to contain "```" inside a string literal (rare but
    // possible) doesn't fool the strip logic — the LAST `\n```` is
    // the closer.
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
    // Scan for the first `{` that opens an object. Anything before
    // it (preamble, fence remnants) is dropped.
    let start = bytes.iter().position(|&b| b == b'{')?;

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut prev_was_backslash = false;
    // Walk char-indices so the returned slice lands on a UTF-8
    // codepoint boundary even when the JSON contains multi-byte
    // characters in string literals (Turkish, emoji, etc.).
    for (idx, ch) in raw[start..].char_indices() {
        let abs = start + idx;
        if in_string {
            if prev_was_backslash {
                // Whatever follows a backslash is consumed literally
                // by JSON parsers; skip it without affecting state.
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
                    // Unbalanced — more `}` than `{`. Bail out so
                    // the caller falls through to the error step.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

/// Truncate `s` to at most `max_chars` Unicode characters. Bounded
/// by `chars()` (not bytes) so the error message never splits on a
/// multi-byte boundary.
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

    fn approved_fixture() -> Verdict {
        Verdict {
            approved: true,
            issues: Vec::new(),
            summary: "OK".to_string(),
        }
    }

    /// Step 1 path — bare JSON object, no fence, no preamble.
    #[test]
    fn parse_verdict_direct_object() {
        let raw = r#"{"approved":true,"issues":[],"summary":"OK"}"#;
        let v = parse_verdict(raw).expect("direct parse");
        assert_eq!(v, approved_fixture());
    }

    /// Step 2 path — language-tagged markdown fence.
    #[test]
    fn parse_verdict_with_json_fence() {
        let raw = "```json\n{\"approved\":true,\"issues\":[],\"summary\":\"OK\"}\n```";
        let v = parse_verdict(raw).expect("fenced parse");
        assert_eq!(v, approved_fixture());
    }

    /// Step 2 path — unlabelled markdown fence.
    #[test]
    fn parse_verdict_with_unlabeled_fence() {
        let raw = "```\n{\"approved\":true,\"issues\":[],\"summary\":\"OK\"}\n```";
        let v = parse_verdict(raw).expect("unlabelled fenced parse");
        assert_eq!(v, approved_fixture());
    }

    /// Step 3 path — preamble before the JSON. Balanced-substring
    /// scan recovers the verdict.
    #[test]
    fn parse_verdict_with_preamble_and_json() {
        let raw = "Here's my verdict for the change:\n{\"approved\":true,\"issues\":[],\"summary\":\"OK\"}";
        let v = parse_verdict(raw).expect("preamble parse");
        assert_eq!(v, approved_fixture());
    }

    /// Rejected verdict with a populated issues list round-trips.
    #[test]
    fn parse_verdict_rejected_with_issues() {
        let raw = r#"{
            "approved": false,
            "issues": [
                {"severity":"high","file":"src/foo.rs","line":42,"msg":"unwrap on None"},
                {"severity":"med","msg":"missing doc comment"}
            ],
            "summary": "Two issues; please fix."
        }"#;
        let v = parse_verdict(raw).expect("issues parse");
        assert!(v.rejected());
        assert_eq!(v.issues.len(), 2);
        assert_eq!(v.issues[0].severity, VerdictSeverity::High);
        assert_eq!(v.issues[0].file.as_deref(), Some("src/foo.rs"));
        assert_eq!(v.issues[0].line, Some(42));
        assert_eq!(v.issues[1].severity, VerdictSeverity::Med);
        assert_eq!(v.issues[1].file, None);
        assert_eq!(v.issues[1].line, None);
    }

    /// All three severity variants serde-roundtrip via the parser.
    #[test]
    fn parse_verdict_severity_variants() {
        for sev in ["high", "med", "low"] {
            let raw = format!(
                r#"{{"approved":false,"issues":[{{"severity":"{sev}","msg":"x"}}],"summary":"s"}}"#
            );
            let v = parse_verdict(&raw)
                .unwrap_or_else(|e| panic!("severity `{sev}` failed: {e:?}"));
            let expected = match sev {
                "high" => VerdictSeverity::High,
                "med" => VerdictSeverity::Med,
                "low" => VerdictSeverity::Low,
                _ => unreachable!(),
            };
            assert_eq!(v.issues[0].severity, expected);
        }
    }

    /// Garbage in → `AppError::SwarmInvoke` out, with the input
    /// preview embedded for diagnostics.
    #[test]
    fn parse_verdict_invalid_returns_error() {
        let err = parse_verdict("lol idk").expect_err("garbage rejected");
        assert_eq!(err.kind(), "swarm_invoke");
        assert!(
            err.message().contains("could not parse Verdict"),
            "error should mention parse failure: {}",
            err.message()
        );
    }

    /// Brace-counting must skip braces inside string literals so a
    /// `{"summary":"a } b"}` with a stray `}` in the summary still
    /// parses via the balanced-substring path.
    #[test]
    fn parse_verdict_balanced_braces_with_strings() {
        // Force the balanced-substring path by prefixing a preamble
        // — the trimmed input is no longer a pure JSON object so
        // step 1 fails and step 3 has to catch.
        let raw = r#"OK here it is: {"approved":true,"issues":[],"summary":"a } b { c"}"#;
        let v = parse_verdict(raw).expect("braced summary parse");
        assert!(v.approved);
        assert_eq!(v.summary, "a } b { c");
    }

    /// Unicode (Turkish + emoji) in the summary survives all four
    /// steps — including the truncation logic in the error path.
    #[test]
    fn parse_verdict_unicode_safe() {
        let raw = r#"{"approved":true,"issues":[],"summary":"İşler yolunda 🚀"}"#;
        let v = parse_verdict(raw).expect("unicode parse");
        assert_eq!(v.summary, "İşler yolunda 🚀");

        // Force the error-path through unicode by sending non-JSON
        // text that still contains multi-byte characters; the 400-
        // char truncation must not panic on a codepoint boundary.
        let garbage =
            "çş".repeat(500) + " not actually json";
        let err = parse_verdict(&garbage).expect_err("garbage rejected");
        // Message contains a prefix of the input but stops at a
        // valid char boundary (no panic, no truncation explosion).
        assert!(err.message().contains("could not parse Verdict"));
    }

    /// `rejected()` is the inverse of `approved`.
    #[test]
    fn verdict_rejected_inverts_approved() {
        let mut v = approved_fixture();
        assert!(!v.rejected());
        v.approved = false;
        assert!(v.rejected());
    }

    /// `strip_markdown_fence` returns `None` on inputs that aren't
    /// fenced, so step 2 falls through cleanly to step 3.
    #[test]
    fn strip_markdown_fence_no_fence_returns_none() {
        assert!(strip_markdown_fence("plain text").is_none());
        assert!(strip_markdown_fence("{\"approved\":true}").is_none());
    }

    /// `first_balanced_json_object` returns `None` when there's no
    /// `{` at all.
    #[test]
    fn first_balanced_json_object_no_brace_returns_none() {
        assert!(first_balanced_json_object("hello world").is_none());
    }

    /// `first_balanced_json_object` recovers the nested object even
    /// when extra junk follows it.
    #[test]
    fn first_balanced_json_object_recovers_with_trailing_junk() {
        let raw = r#"prefix {"a":{"b":1}} trailing junk"#;
        let inner = first_balanced_json_object(raw)
            .expect("balanced object found");
        assert_eq!(inner, r#"{"a":{"b":1}}"#);
    }
}

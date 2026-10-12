//! Shared LLM-output JSON extraction helpers.
//!
//! Every swarm-side parser that consumes raw `claude` assistant text
//! (reviewer verdict, coordinator decision, orchestrator outcome,
//! help request, brain action) walks the same defense-in-depth
//! recipe — real LLMs wrap output in markdown fences or prepend
//! conversational preambles despite strict output contracts:
//!
//!   1. whole text is JSON
//!   2. fence-stripped ([`strip_fence`])
//!   3. first balanced `{...}` substring ([`first_balanced_object`])
//!   4. bail (parser-specific: `None` or a structured error)
//!
//! Steps 2 and 3 used to be copy-pasted in five modules with two
//! slightly divergent dialects; this is the single owner now.
//! Unified semantics (the most tolerant of the two dialects — any
//! input the old variants accepted still parses, because step 3
//! backstops step 2):
//!
//! - the opening fence may appear anywhere in the text (preamble
//!   before ``` is fine);
//! - the LAST ``` after the opening fence closes it, so a stray
//!   ``` inside a JSON string literal can't truncate the block;
//! - the inner text is returned raw (no trim) — serde_json accepts
//!   surrounding whitespace, and callers that need trimming trim.

/// Strip the first ```json ... ``` (or ``` ... ```) fence in `s` and
/// return the raw inner contents. `None` when no complete fence is
/// present.
pub(crate) fn strip_fence(s: &str) -> Option<&str> {
    let start_idx = s.find("```")?;
    let after_open = &s[start_idx + 3..];
    // Optional language tag runs to the first newline; a fence with
    // no newline at all (```{...}```) keeps everything after the
    // backticks.
    let after_lang = match after_open.find('\n') {
        Some(n) => &after_open[n + 1..],
        None => after_open,
    };
    let close_idx = after_lang.rfind("```")?;
    Some(&after_lang[..close_idx])
}

/// Find the first balanced `{...}` substring in `raw`. String-aware:
/// a `{` or `}` inside a `"..."` literal does not affect the depth
/// counter, and a backslash-escaped `\"` inside that literal does
/// not close the string. Walks char-indices so the returned slice
/// always lands on a UTF-8 codepoint boundary (Turkish text, emoji).
///
/// Returns `None` if there is no `{` at all, or if the input is
/// unbalanced (more `}` than `{`, or unclosed at end-of-input).
pub(crate) fn first_balanced_object(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;

    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut prev_was_backslash = false;
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
                    // Unbalanced — more `}` than `{`. Bail out so the
                    // caller falls through to its error step.
                    return None;
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{first_balanced_object, strip_fence};

    #[test]
    fn strip_fence_extracts_inner_content() {
        let s = "before\n```json\n{\"x\":1}\n```\nafter";
        assert_eq!(strip_fence(s), Some("{\"x\":1}\n"));
    }

    #[test]
    fn strip_fence_returns_none_without_fence() {
        assert!(strip_fence("no fences here").is_none());
        assert!(strip_fence("{\"approved\":true}").is_none());
    }

    #[test]
    fn strip_fence_last_closer_wins() {
        // A ``` inside the block must not truncate it — the LAST
        // closer is authoritative.
        let s = "```json\n{\"note\":\"see ``` markers\"}\n```";
        assert_eq!(strip_fence(s), Some("{\"note\":\"see ``` markers\"}\n"));
    }

    #[test]
    fn first_balanced_object_handles_strings_with_braces() {
        let s = r#"hi {"key": "value with } inside", "n": 1} bye"#;
        assert_eq!(
            first_balanced_object(s),
            Some(r#"{"key": "value with } inside", "n": 1}"#)
        );
    }

    #[test]
    fn first_balanced_object_no_brace_returns_none() {
        assert!(first_balanced_object("hello world").is_none());
    }

    #[test]
    fn first_balanced_object_recovers_with_trailing_junk() {
        let raw = "Sure! Here is the verdict: {\"approved\":true,\"issues\":[],\"summary\":\"ok\"} hope that helps";
        let inner = first_balanced_object(raw).expect("recovers object");
        assert!(inner.starts_with('{') && inner.ends_with('}'));
    }

    #[test]
    fn first_balanced_object_multibyte_boundary_is_safe() {
        let s = "ön söz {\"k\":\"şü😀\"} son";
        assert_eq!(first_balanced_object(s), Some("{\"k\":\"şü😀\"}"));
    }
}

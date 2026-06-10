//! Awaiting-approval detection.
//!
//! Per-agent regex sets that recognise the canonical "this tool needs
//! your approval" prompts, plus a best-effort extractor for the
//! structured `ApprovalBanner` blob surfaced above an
//! `awaiting_approval` pane. Per WP-W2-06 § "Acceptance criteria" and
//! NEURON_TERMINAL_REPORT § state machine.

use std::sync::OnceLock;

use regex::Regex;

use crate::models::ApprovalBanner;

/// Best-effort extractor for the `ApprovalBanner` blob shown above an
/// `awaiting_approval` pane. Week 2 minimum: tries one structured
/// regex against `claude-code` output and falls back to a placeholder
/// `{tool: "unknown", target: "", added: 0, removed: 0}` for all other
/// agents (and for claude-code when the structured form does not
/// match). Real CLIs do not yet emit a stable machine-readable
/// approval block, so the placeholder is what the UI sees most of the
/// time — the field merely needs to be non-null to trigger the amber
/// banner.
pub(super) fn extract_approval_blob(agent_kind: &str, text: &str) -> ApprovalBanner {
    fn placeholder() -> ApprovalBanner {
        ApprovalBanner {
            tool: "unknown".into(),
            target: String::new(),
            added: 0,
            removed: 0,
        }
    }
    if agent_kind == "claude-code" {
        // Brief §1.4: structured form. `(?ms)` so `.` spans newlines.
        static RE: OnceLock<Regex> = OnceLock::new();
        let re = RE.get_or_init(|| {
            Regex::new(
                r"(?ms)^Tool:\s*(?P<tool>\S+).*?target:\s*(?P<target>\S+).*?\+(?P<add>\d+).*?-(?P<rem>\d+)",
            )
            .expect("claude approval blob regex")
        });
        if let Some(caps) = re.captures(text) {
            let tool = caps.name("tool").map(|m| m.as_str().to_string()).unwrap_or_default();
            let target = caps.name("target").map(|m| m.as_str().to_string()).unwrap_or_default();
            let added: i64 = caps
                .name("add")
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            let removed: i64 = caps
                .name("rem")
                .and_then(|m| m.as_str().parse().ok())
                .unwrap_or(0);
            return ApprovalBanner { tool, target, added, removed };
        }
    }
    placeholder()
}

/// Dispatch table for awaiting-approval detection. One regex set per
/// agent kind, lazily compiled on first use. Per WP-W2-06 §
/// "Acceptance criteria" and NEURON_TERMINAL_REPORT § state machine.
pub(super) fn matches_awaiting_approval(agent_kind: &str, text: &str) -> bool {
    let regexes = match agent_kind {
        "claude-code" => claude_regexes(),
        "codex" => codex_regexes(),
        "gemini" => gemini_regexes(),
        _ => return false,
    };
    regexes.iter().any(|re| re.is_match(text))
}

fn claude_regexes() -> &'static [Regex] {
    static CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
    CACHE.get_or_init(|| {
        vec![
            // Trailing prompt question, e.g. "Do you want to approve this?"
            Regex::new(r"(?m)Approve.*\?$").expect("claude approve regex"),
            // Tool approval banner.
            Regex::new(r"(?m)^Tool: .* needs approval").expect("claude tool regex"),
        ]
    })
}

fn codex_regexes() -> &'static [Regex] {
    static CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
    CACHE.get_or_init(|| {
        vec![Regex::new(r"(?m)Apply this patch\? \[y/n\]").expect("codex regex")]
    })
}

fn gemini_regexes() -> &'static [Regex] {
    static CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
    CACHE.get_or_init(|| vec![Regex::new(r"(?m)^\[awaiting\]").expect("gemini regex")])
}

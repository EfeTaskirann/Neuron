//! Per-agent awaiting-approval regex coverage.
use super::super::approval::matches_awaiting_approval;

/// Acceptance: awaiting-approval regex matches each canonical agent
/// prompt.
#[test]
fn awaiting_approval_regex_matches_canonical_prompts() {
    // Claude Code — "Approve … ?" form.
    let claude_a = "Tool wants to write file foo.txt\nApprove this change?";
    assert!(matches_awaiting_approval("claude-code", claude_a));

    // Claude Code — "Tool: ... needs approval" form.
    let claude_b = "Tool: Write needs approval\nfile=/tmp/x";
    assert!(matches_awaiting_approval("claude-code", claude_b));

    // Codex — "Apply this patch?".
    let codex = "diff --git a/foo b/foo\nApply this patch? [y/n]";
    assert!(matches_awaiting_approval("codex", codex));

    // Gemini — "[awaiting]" line marker.
    let gemini = "Some preceding output\n[awaiting] user input";
    assert!(matches_awaiting_approval("gemini", gemini));

    // Plain shell never matches.
    assert!(!matches_awaiting_approval("shell", claude_a));

    // Unrelated text doesn't fire any regex.
    assert!(!matches_awaiting_approval("claude-code", "Just running ls"));
}

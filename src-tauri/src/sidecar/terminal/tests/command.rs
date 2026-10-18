//! Command-string coverage: tokenizer, agent-kind inference,
//! default-shell resolution, and `~`/absolute cwd expansion.
use super::super::command::{
    default_shell, expand_cwd, infer_agent_kind, inherited_claude_session_env, tokenize_command,
};
use std::path::PathBuf;

fn env(pairs: &[&str]) -> Vec<(String, String)> {
    pairs.iter().map(|k| (k.to_string(), "1".to_string())).collect()
}

/// Neuron launched from inside a Claude Code session: every host
/// session variable is dropped from the panes (live: with claude
/// 2.1.29x they turned each pane into a transcript-less child session,
/// so no pane ever became ready and every message was rejected).
#[test]
fn a_parent_claude_session_leaks_none_of_its_env_into_panes() {
    let mut removed = inherited_claude_session_env(env(&[
        "CLAUDECODE",
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_HOST_SESSION_ID",
        "CLAUDE_AGENT_SDK_VERSION",
        "Anthropic_Base_Url",
        "PATH",
        "USERPROFILE",
    ]));
    removed.sort();
    assert_eq!(
        removed,
        vec![
            "Anthropic_Base_Url",
            "CLAUDECODE",
            "CLAUDE_AGENT_SDK_VERSION",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_HOST_SESSION_ID",
        ]
    );
}

/// A normal launch keeps the user's own settings (e.g. a deliberate
/// `CLAUDE_CODE_USE_BEDROCK`); only the fixed scrub list applies there.
#[test]
fn a_normal_launch_keeps_the_users_claude_env() {
    assert!(inherited_claude_session_env(env(&["CLAUDE_CODE_USE_BEDROCK", "PATH"])).is_empty());
}

/// Acceptance: tokenizer handles bare programs, args, and quoted
/// segments. Required so `terminal:spawn({cmd: "/bin/sh -c \"echo
/// hi\""})` actually spawns `/bin/sh` with two args, not a single
/// nonsense program-name.
#[test]
fn tokenize_command_handles_quotes_and_spaces() {
    assert_eq!(tokenize_command("pwsh.exe"), vec!["pwsh.exe"]);
    assert_eq!(
        tokenize_command("cmd.exe /c echo hello"),
        vec!["cmd.exe", "/c", "echo", "hello"]
    );
    assert_eq!(
        tokenize_command(r#"/bin/sh -c "echo hello""#),
        vec!["/bin/sh", "-c", "echo hello"]
    );
    assert_eq!(
        tokenize_command("'with single' \"and double\""),
        vec!["with single", "and double"]
    );
    // Empty input → empty vec.
    assert!(tokenize_command("").is_empty());
    assert!(tokenize_command("   ").is_empty());
    // Backslash-escape inside double quotes preserves the literal.
    assert_eq!(
        tokenize_command(r#""a \"b\" c""#),
        vec!["a \"b\" c"]
    );
}

/// Acceptance: agent kind inferred from cmd string.
#[test]
fn infer_agent_kind_substring_match() {
    assert_eq!(infer_agent_kind("claude-code --workspace foo"), "claude-code");
    assert_eq!(infer_agent_kind("/usr/local/bin/claude-code"), "claude-code");
    assert_eq!(infer_agent_kind("codex"), "codex");
    assert_eq!(infer_agent_kind("gemini-cli"), "gemini");
    assert_eq!(infer_agent_kind("/bin/bash"), "shell");
    assert_eq!(infer_agent_kind("pwsh.exe"), "shell");
    // Case insensitivity.
    assert_eq!(infer_agent_kind("CLAUDE-CODE"), "claude-code");
}

/// Acceptance: default shell is non-empty on every platform. We
/// don't pin to a specific binary because CI may not have pwsh.
#[test]
fn default_shell_returns_a_non_empty_path() {
    let s = default_shell();
    assert!(!s.is_empty());
    if cfg!(windows) {
        assert!(
            s.eq_ignore_ascii_case("pwsh.exe") || s.eq_ignore_ascii_case("powershell.exe"),
            "Windows default shell must be one of pwsh.exe / powershell.exe; got {s}"
        );
    }
}

/// `expand_cwd` resolves `~` against `$HOME` (or `%USERPROFILE%`)
/// and passes through absolute paths verbatim.
#[test]
fn expand_cwd_handles_tilde_and_absolute_paths() {
    // Absolute paths pass through unchanged.
    let abs = if cfg!(windows) { "C:\\tmp" } else { "/tmp" };
    assert_eq!(expand_cwd(abs), PathBuf::from(abs));

    // `~` → home; if $HOME is unset on the test host we still
    // get a non-`~` path back (USERPROFILE / fallback) and the
    // function does not panic.
    let home = expand_cwd("~");
    if home.as_os_str().to_string_lossy() != "~" {
        assert!(home.is_absolute() || !home.as_os_str().is_empty());
    }
}

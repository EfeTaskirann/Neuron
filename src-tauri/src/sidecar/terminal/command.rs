//! Command-line / shell resolution helpers for `spawn_pane`.
//!
//! Pure functions: home-directory expansion, default-shell discovery,
//! a POSIX-style tokenizer, agent-kind inference, and the env-scrub
//! list applied to freshly spawned `claude-code` panes.

use std::path::PathBuf;

/// Env vars stripped from the inherited environment of a freshly
/// spawned `claude-code` pane. Four categories:
///
/// 1. **Nested-instance signals** (`CLAUDECODE`, `CLAUDE_CODE_SESSION_ID`,
///    `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`, `CLAUDE_CODE_AGENT`,
///    `CLAUDE_EFFORT`, `AI_AGENT`) — set by an outer `claude` shell to
///    flag "this child is nested". Without stripping, the spawned REPL
///    falls into the OAuth login picker on every spawn.
///
/// 2. **Auth-bypass tokens / endpoint overrides** (`ANTHROPIC_API_KEY`,
///    `ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`,
///    `ANTHROPIC_BASE_URL`, `ANTHROPIC_API_URL`,
///    `ANTHROPIC_CUSTOM_HEADERS`) — any of these, when inherited from
///    the parent process, override the per-pane `.credentials.json` we
///    seed into each isolated HOME. The 2026-05-13 `/login` regression
///    pinned this to `CLAUDE_CODE_OAUTH_TOKEN`: when Neuron is launched
///    from inside a Claude Code shell (the user's typical dev loop),
///    Claude Code exports its session token here for nested processes.
///    Each spawned pane then prefers the parent's token over its own
///    seeded credentials, finds it scoped to a different session, and
///    drops into `/login`. Stripping all six forces the pane to use
///    the local `.credentials.json` we control.
///
/// 3. **Provider-routing toggles** (`USE_BEDROCK`, `USE_VERTEX`,
///    `USE_FOUNDRY`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
///    `CLAUDE_CODE_SKIP_BEDROCK_AUTH`, `CLAUDE_CODE_SKIP_VERTEX_AUTH`)
///    — silently flip the spawn off the user's Pro/Max OAuth channel
///    onto BYOK billing or a different cloud provider, breaking the
///    subscription-only contract mirrored from
///    `swarm::binding::STRIPPED_ENV_VARS`.
///
/// 4. **Config-dir override** (`CLAUDE_CONFIG_DIR`) — if set in the
///    parent, every pane ignores its per-pane HOME and races on the
///    shared real config dir, undoing the isolation that prevents
///    `.claude.json` truncation under 9-way concurrent writes.
pub(super) const CLAUDE_AGENT_STRIPPED_ENV: &[&str] = &[
    // Nested-instance signals.
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_AGENT",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    // Auth-bypass tokens / endpoint overrides.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    // Provider-routing toggles.
    "USE_BEDROCK",
    "USE_VERTEX",
    "USE_FOUNDRY",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    // Config-dir override.
    "CLAUDE_CONFIG_DIR",
];

/// Best-effort home directory expansion for `cwd`. `~` and `~/...`
/// resolve against the current user's home; other paths pass through.
pub(super) fn expand_cwd(input: &str) -> PathBuf {
    if input == "~" {
        return home_dir_or(input);
    }
    if let Some(rest) = input.strip_prefix("~/") {
        let mut p = home_dir_or(rest);
        if rest.is_empty() {
            return p;
        }
        if p.as_os_str().is_empty() {
            return PathBuf::from(input);
        }
        p.push(rest);
        return p;
    }
    PathBuf::from(input)
}

fn home_dir_or(fallback: &str) -> PathBuf {
    // Cross-platform home discovery without pulling a new dependency:
    // standard env vars on each OS. We fall back to the literal input
    // (the shell can deal with `~`) on the failure path so an
    // unconfigured CI box doesn't blow up.
    if let Some(p) = std::env::var_os("HOME") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if cfg!(windows) {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            if !p.is_empty() {
                return PathBuf::from(p);
            }
        }
    }
    PathBuf::from(fallback)
}

/// Resolve the platform default shell.
///
/// - Windows: `pwsh.exe` if discoverable on `PATH` via `where.exe`,
///   else `powershell.exe`.
/// - Unix: `$SHELL` if set, else `/bin/sh`.
pub(super) fn default_shell() -> String {
    #[cfg(windows)]
    {
        if has_pwsh() {
            return "pwsh.exe".into();
        }
        return "powershell.exe".into();
    }
    #[cfg(not(windows))]
    {
        if let Ok(s) = std::env::var("SHELL") {
            if !s.is_empty() {
                return s;
            }
        }
        "/bin/sh".into()
    }
}

/// POSIX-style tokenizer for command strings. Handles single- and
/// double-quoted segments and backslash escapes inside double quotes.
/// Sufficient for the WP-W2-06 `cmd` field, which is either a bare
/// program name (`pwsh.exe`) or a small shell-style invocation
/// (`/bin/sh -c "echo hello"`). Not a full POSIX `sh` parser — anyone
/// needing pipes / globs / env expansion should pass them inside the
/// shell child, not in the spawn command.
pub(super) fn tokenize_command(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut chars = input.chars().peekable();
    let mut have_token = false;
    while let Some(c) = chars.next() {
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                cur.push(c);
            }
            have_token = true;
            continue;
        }
        if in_double {
            if c == '\\' {
                if let Some(&next) = chars.peek() {
                    if next == '"' || next == '\\' {
                        cur.push(next);
                        chars.next();
                        continue;
                    }
                }
                cur.push(c);
            } else if c == '"' {
                in_double = false;
            } else {
                cur.push(c);
            }
            have_token = true;
            continue;
        }
        match c {
            '\'' => {
                in_single = true;
                have_token = true;
            }
            '"' => {
                in_double = true;
                have_token = true;
            }
            ' ' | '\t' => {
                if have_token {
                    out.push(std::mem::take(&mut cur));
                    have_token = false;
                }
            }
            _ => {
                cur.push(c);
                have_token = true;
            }
        }
    }
    if have_token {
        out.push(cur);
    }
    out
}

#[cfg(windows)]
fn has_pwsh() -> bool {
    // Best-effort PATH scan — `Command::new("where.exe").arg("pwsh.exe")`
    // succeeds with exit code 0 if PATH contains pwsh. We do NOT use
    // the .status() call inside an async context here; this function is
    // called from `spawn_pane` which is async, but the call itself is a
    // short-lived sync OS call, which is fine.
    match std::process::Command::new("where.exe")
        .arg("pwsh.exe")
        .output()
    {
        Ok(out) => out.status.success(),
        Err(_) => false,
    }
}

/// Infer the agent kind ("claude-code"/"codex"/"gemini"/"shell") from
/// a command string. Substring match — robust to absolute paths
/// (`/usr/local/bin/claude-code`) and to commands with arguments
/// (`claude-code --workspace x`).
pub(super) fn infer_agent_kind(cmd: &str) -> &'static str {
    let lower = cmd.to_lowercase();
    if lower.contains("claude-code") {
        "claude-code"
    } else if lower.contains("codex") {
        "codex"
    } else if lower.contains("gemini") {
        "gemini"
    } else {
        "shell"
    }
}

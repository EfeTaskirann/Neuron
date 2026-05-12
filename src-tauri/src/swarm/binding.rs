//! `claude` CLI invocation helpers (WP-W3-11 §3).
//!
//! Three responsibilities:
//!
//! 1. **Resolution** of the host's `claude` binary path. Mirrors
//!    `crate::sidecar::agent::resolve_python`'s 3-step pattern:
//!    explicit env override → `which` PATH lookup → platform-specific
//!    fallback locations.
//! 2. **Subscription-only env** for the spawned subprocess. The
//!    Phase 1 transport must run on the user's Pro / Max OAuth
//!    channel; an injected `ANTHROPIC_API_KEY` would silently flip
//!    `claude` into BYOK billing. Strip it (and the three provider-
//!    routing toggles) so the subprocess inherits everything else
//!    verbatim.
//! 3. **argv builder** for a one-shot per-invoke specialist call. The
//!    flag order is the contract from WP §3 — do not deviate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::swarm::profile::{PermissionMode, Profile};

/// Env var names stripped from the spawned process so the `claude`
/// CLI cannot fall back to API-key auth or a non-Anthropic provider.
/// Documented at `report/Neuron Multi-Agent Orchestration ...` §3.4.
const STRIPPED_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "USE_BEDROCK",
    "USE_VERTEX",
    "USE_FOUNDRY",
];

/// Env var name a developer / CI run sets to override the resolved
/// `claude` binary path (test fixture, custom install, etc.).
pub const CLAUDE_BIN_ENV: &str = "NEURON_CLAUDE_BIN";

/// Result of resolving the `claude` binary on this host. Carries only
/// the absolute path; everything else (env, args) is built per-invoke.
#[derive(Debug, Clone)]
pub struct ClaudeBinary {
    pub path: PathBuf,
}

/// PTY-friendly spawn specification.
///
/// On Windows, claude is most often installed via `npm i -g
/// @anthropic-ai/claude-code`, which drops a `claude.cmd` batch
/// wrapper. portable-pty + ConPTY is known to mis-handle .cmd
/// wrappers' detach trick (`endLocal & goto #_undefined_#`), producing
/// silent panes with no banner / prompt output. When we detect the
/// npm install layout (a `cli.js` next to `claude.cmd`), we spawn
/// `node.exe cli.js` directly to bypass the wrapper.
///
/// On Unix or when the wrapper bypass isn't applicable, `program`
/// is just the resolved claude path and `prefix_args` is empty.
#[derive(Debug, Clone)]
pub struct ClaudeSpawn {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

/// Resolve a PTY-safe invocation spec for the claude CLI.
///
/// On Windows, the npm-installed package (`@anthropic-ai/claude-code`)
/// ships a native `claude.exe` under
/// `<npm-root>/node_modules/@anthropic-ai/claude-code/bin/claude.exe`,
/// and `claude.cmd` is just a thin batch wrapper that invokes it.
/// We resolve through to the underlying .exe so portable-pty / ConPTY
/// owns the child directly — the batch wrapper's `endLocal` detach
/// trick breaks PTY handle inheritance and silences output.
pub fn resolve_claude_spawn() -> Result<ClaudeSpawn, AppError> {
    let binary = resolve_claude_binary()?;
    if cfg!(target_os = "windows")
        && binary
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.eq_ignore_ascii_case("cmd"))
            .unwrap_or(false)
    {
        if let Some(parent) = binary.path.parent() {
            // npm install layout (v2.x): native binary under bin/.
            let native_exe = parent
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude.exe");
            if native_exe.is_file() {
                return Ok(ClaudeSpawn {
                    program: native_exe,
                    prefix_args: vec![],
                });
            }
            // Legacy npm install layout (v1.x): node + cli.js shim.
            let cli_js = parent
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-code")
                .join("cli.js");
            if cli_js.is_file() {
                let co_located_node = parent.join("node.exe");
                let node_program = if co_located_node.is_file() {
                    co_located_node
                } else {
                    which::which("node")
                        .unwrap_or_else(|_| PathBuf::from("node.exe"))
                };
                return Ok(ClaudeSpawn {
                    program: node_program,
                    prefix_args: vec![cli_js.display().to_string()],
                });
            }
        }
    }
    Ok(ClaudeSpawn {
        program: binary.path,
        prefix_args: vec![],
    })
}

/// Locate the `claude` CLI. See WP §3 for the resolution order:
///
/// 1. `NEURON_CLAUDE_BIN` env var — explicit override.
/// 2. `which::which("claude")` — covers macOS Homebrew, Linux package
///    managers, and Windows after the official installer drops
///    `claude.cmd` on PATH.
/// 3. Platform-specific common locations:
///    - Windows: `%LOCALAPPDATA%\Programs\claude\claude.cmd`
///    - macOS:   `~/.npm-global/bin/claude`
///    - Linux:   `~/.local/bin/claude`
///
/// Misses produce an `AppError::ClaudeBinaryMissing` whose message
/// embeds *what we tried* so the user sees a CTA pointing at the
/// official setup docs without us shipping a separate diagnostic.
pub fn resolve_claude_binary() -> Result<ClaudeBinary, AppError> {
    let mut tried: Vec<String> = Vec::new();

    // 1. Explicit env override.
    match std::env::var(CLAUDE_BIN_ENV) {
        Ok(p) if !p.trim().is_empty() => {
            let path = PathBuf::from(p.trim());
            if path.is_file() {
                return Ok(ClaudeBinary { path });
            }
            tried.push(format!("${CLAUDE_BIN_ENV}=`{}` (not a file)", path.display()));
        }
        _ => tried.push(format!("${CLAUDE_BIN_ENV} (unset)")),
    }

    // 2. PATH lookup via `which`.
    //
    // On Windows, the first PATH match is often the Microsoft Store
    // **App Execution Alias** at `%LOCALAPPDATA%\Microsoft\WindowsApps\
    // claude.cmd` — a stub that silently exits when the underlying
    // Store package isn't installed. Skip it explicitly so the real
    // npm install at `%APPDATA%\npm\claude.cmd` (next on PATH) wins.
    match which::which("claude") {
        Ok(path) => {
            let lower = path.to_string_lossy().to_lowercase();
            if cfg!(target_os = "windows")
                && lower.contains("\\windowsapps\\")
            {
                tried.push(format!(
                    "which::which(\"claude\") → {} (skipped: Microsoft \
                     Store app execution alias stub)",
                    path.display()
                ));
            } else {
                return Ok(ClaudeBinary { path });
            }
        }
        Err(e) => tried.push(format!("which::which(\"claude\") → {e}")),
    }

    // 3. Platform-specific fallbacks.
    for candidate in platform_fallback_paths() {
        if candidate.is_file() {
            return Ok(ClaudeBinary { path: candidate });
        }
        tried.push(format!("{} (not present)", candidate.display()));
    }

    Err(AppError::ClaudeBinaryMissing(format!(
        "could not locate the `claude` CLI on this host. \
         Tried: [{}]. Install Claude Code per \
         https://docs.claude.com/en/docs/claude-code/setup",
        tried.join("; ")
    )))
}

/// Platform-specific fallback paths probed after `which` misses.
/// Pulled into a separate fn so the resolution chain is testable.
fn platform_fallback_paths() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        // npm global install (most common) — `npm i -g @anthropic-ai/claude-code`
        // drops the .cmd shim into %APPDATA%\npm\.
        if let Ok(roaming) = std::env::var("APPDATA") {
            out.push(
                PathBuf::from(roaming).join("npm").join("claude.cmd"),
            );
        }
        // Anthropic standalone installer drop point.
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            out.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("claude")
                    .join("claude.cmd"),
            );
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = home_dir() {
            out.push(home.join(".npm-global").join("bin").join("claude"));
        }
    } else {
        // Linux + everything else (BSDs, etc.) — the conventional
        // `~/.local/bin/claude` lookup; harmless on hosts where it
        // doesn't exist.
        if let Some(home) = home_dir() {
            out.push(home.join(".local").join("bin").join("claude"));
        }
    }
    out
}

/// Best-effort home dir. Falls back to `USERPROFILE` on Windows when
/// `HOME` is unset (Tauri sometimes runs without `HOME` propagated).
fn home_dir() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h));
        }
    }
    if cfg!(target_os = "windows") {
        if let Ok(h) = std::env::var("USERPROFILE") {
            if !h.is_empty() {
                return Some(PathBuf::from(h));
            }
        }
    }
    None
}

/// Build the env map for a `claude` spawn. Inherits the parent's env
/// minus the four auth-routing variables in `STRIPPED_ENV_VARS` so the
/// child must use whatever subscription session `~/.claude/.credentials`
/// already holds.
pub fn subscription_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for var in STRIPPED_ENV_VARS {
        env.remove(*var);
    }
    env
}

/// Build the argv for a one-shot per-invoke specialist call. The flag
/// order is the contract from WP §3:
///
/// ```text
/// -p
/// --input-format stream-json
/// --output-format stream-json
/// --verbose
/// --append-system-prompt-file <system_prompt_file>
/// --max-turns <profile.max_turns>
/// (--permission-mode plan)            -- only when permission_mode == Plan
/// (--dangerously-skip-permissions)    -- only when permission_mode != Plan
/// --allowedTools "<comma-joined profile.allowed_tools>"
/// ```
///
/// `--system-prompt` and `--system-prompt-file` (replace mode) are
/// **never** emitted — only `--append-system-prompt-file`. Replacing
/// the system prompt would drop Claude Code's default tool-use
/// conditioning, which the persona depends on (WP §"Hard rules").
pub fn build_specialist_args(
    profile: &Profile,
    system_prompt_file: &Path,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--append-system-prompt-file".into(),
        system_prompt_file.to_string_lossy().into_owned(),
        "--max-turns".into(),
        profile.max_turns.to_string(),
    ];

    match profile.permission_mode {
        PermissionMode::Plan => {
            // Plan mode: explicit `--permission-mode plan`; the
            // dangerous-skip flag is omitted so prompted approvals
            // still surface. The Coordinator (W3-12) will catch them.
            args.push("--permission-mode".into());
            args.push("plan".into());
        }
        PermissionMode::AcceptEdits | PermissionMode::AcceptAll => {
            // Phase 1 binary gate per WP §3 "Permissions note":
            // anything past `Plan` skips prompts wholesale so the
            // smoke command can run without a UI. W3-12 splits these
            // into per-tool allow / deny lists.
            args.push("--dangerously-skip-permissions".into());
        }
    }

    args.push("--allowedTools".into());
    args.push(profile.allowed_tools.join(","));
    args
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    //! `subscription_env()` reads `std::env::vars()` at call time, so
    //! the strip-tests mutate process-global env. Each test owns its
    //! own variable name (`NEURON_TEST_SE_*`) to avoid races with
    //! other tests in the suite, then restores the prior value at end.

    use super::*;
    use crate::swarm::profile::PermissionMode;
    use std::path::PathBuf;

    fn fixture_profile(mode: PermissionMode) -> Profile {
        Profile {
            id: "test".into(),
            version: "1.0.0".into(),
            role: "Test".into(),
            description: "Test".into(),
            allowed_tools: vec!["Read".into(), "Grep".into()],
            permission_mode: mode,
            max_turns: 7,
            body: "persona body".into(),
            source_path: PathBuf::from("test.md"),
        }
    }

    /// Save+set+restore an env var for the duration of a closure.
    /// Process-global, so callers picking unique names are responsible
    /// for thread safety of *that* variable.
    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let prior = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match prior {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn subscription_env_strips_api_key() {
        with_env("ANTHROPIC_API_KEY", Some("sk-test-fake"), || {
            let env = subscription_env();
            assert!(
                !env.contains_key("ANTHROPIC_API_KEY"),
                "ANTHROPIC_API_KEY must not survive subscription_env()"
            );
        });
    }

    #[test]
    fn subscription_env_strips_provider_routes() {
        with_env("USE_BEDROCK", Some("1"), || {
            with_env("USE_VERTEX", Some("1"), || {
                with_env("USE_FOUNDRY", Some("1"), || {
                    let env = subscription_env();
                    for var in ["USE_BEDROCK", "USE_VERTEX", "USE_FOUNDRY"] {
                        assert!(
                            !env.contains_key(var),
                            "{var} must not survive subscription_env()"
                        );
                    }
                });
            });
        });
    }

    /// Pass-through env vars survive (negative control for the strip).
    #[test]
    fn subscription_env_preserves_other_vars() {
        let key = "NEURON_TEST_SE_PASSTHROUGH";
        with_env(key, Some("hello"), || {
            let env = subscription_env();
            assert_eq!(env.get(key).map(String::as_str), Some("hello"));
        });
    }

    /// WP §7 — argv carries the required flags and never the replace-
    /// mode system-prompt flags.
    #[test]
    fn specialist_args_contain_required_flags() {
        let profile = fixture_profile(PermissionMode::AcceptEdits);
        let args = build_specialist_args(
            &profile,
            Path::new("/tmp/sys-prompt.md"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("-p"));
        assert!(joined.contains("--input-format stream-json"));
        assert!(joined.contains("--output-format stream-json"));
        assert!(joined.contains("--append-system-prompt-file"));
        assert!(joined.contains("--max-turns 7"));
        assert!(joined.contains("--allowedTools Read,Grep"));
        // Replace-mode flags must NEVER appear.
        assert!(
            !args.iter().any(|a| a == "--system-prompt"),
            "--system-prompt is forbidden"
        );
        assert!(
            !args.iter().any(|a| a == "--system-prompt-file"),
            "--system-prompt-file (replace mode) is forbidden"
        );
    }

    /// WP §7 — Plan mode emits `--permission-mode plan` and skips the
    /// dangerous flag.
    #[test]
    fn plan_mode_skips_dangerous_flag() {
        let profile = fixture_profile(PermissionMode::Plan);
        let args = build_specialist_args(
            &profile,
            Path::new("/tmp/sys-prompt.md"),
        );
        assert!(args.iter().any(|a| a == "--permission-mode"));
        assert!(args.iter().any(|a| a == "plan"));
        assert!(
            !args.iter().any(|a| a == "--dangerously-skip-permissions"),
            "Plan mode must NOT include --dangerously-skip-permissions"
        );
    }

    /// AcceptAll mirrors AcceptEdits in Phase 1 (binary gate).
    #[test]
    fn accept_all_mode_emits_dangerous_flag() {
        let profile = fixture_profile(PermissionMode::AcceptAll);
        let args = build_specialist_args(
            &profile,
            Path::new("/tmp/sys-prompt.md"),
        );
        assert!(args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(
            !args.iter().any(|a| a == "--permission-mode"),
            "non-plan modes must NOT carry --permission-mode"
        );
    }
}

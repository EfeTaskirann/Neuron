//! Resolution of the host's `claude` binary and its PTY-safe spawn
//! spec. See the module-level docs (`super`) for the responsibility
//! split; this file owns responsibility #1 (binary/spawn resolution).

use std::path::PathBuf;

use crate::error::AppError;

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

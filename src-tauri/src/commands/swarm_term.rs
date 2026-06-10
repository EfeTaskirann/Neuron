//! Terminal-Hierarchy Swarm IPC surface.
//!
//! Commands: `swarm_term_list_personas` (bundled persona metadata),
//! `swarm_term_session_status`, `swarm_term_start_session`,
//! `swarm_term_stop_session`, and `swarm_term_run_update` (update the
//! host `claude` CLI). Inter-agent routing is event-driven via the
//! file-IPC bridge (`swarm_term::bridge`), emitted as `swarm-term:route`
//! / `swarm-term:lifecycle` events — not a command.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::error::AppError;
use crate::swarm::binding::{resolve_claude_spawn, subscription_env};
use crate::swarm::ProfileRegistry;
use crate::swarm_term::TerminalSwarmSessionHandle;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SwarmTermPersona {
    pub id: String,
    pub role: String,
    pub description: String,
    pub allowed_destinations: Vec<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_list_personas<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Vec<SwarmTermPersona>, AppError> {
    let workspace_dir = app
        .path()
        .app_data_dir()
        .ok()
        .map(|p| p.join("swarm-term").join("agents"))
        .filter(|p| p.is_dir());
    let registry = ProfileRegistry::load_term(workspace_dir.as_deref())?;
    let mut out: Vec<SwarmTermPersona> = registry
        .list()
        .into_iter()
        .filter(|p| crate::swarm_term::hierarchy::AGENT_IDS.contains(&p.id.as_str()))
        .map(|p| SwarmTermPersona {
            id: p.id.clone(),
            role: p.role.clone(),
            description: p.description.clone(),
            allowed_destinations: crate::swarm_term::hierarchy::allowed_for(&p.id)
                .iter()
                .map(|s| s.to_string())
                .collect(),
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_session_status<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Option<TerminalSwarmSessionHandle>, AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>();
    Ok(registry.current())
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_term_start_session<R: Runtime>(
    app: AppHandle<R>,
    project_dir: String,
) -> Result<TerminalSwarmSessionHandle, AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>()
        .inner()
        .clone();
    let path = std::path::PathBuf::from(project_dir.trim());
    registry.start(app.clone(), path).await
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_stop_session<R: Runtime>(
    app: AppHandle<R>,
) -> Result<(), AppError> {
    let registry = app
        .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>()
        .inner()
        .clone();
    registry.stop(app.clone()).await
}

/// Result returned by `swarm_term_run_update` once the update child has
/// exited. The tails are bounded so the frontend can show a final
/// success / failure summary without the full transcript — the live
/// stream is delivered as `swarm-term:update:log` events.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUpdateResult {
    pub exit_code: i32,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

const UPDATE_LOG_EVENT: &str = crate::events::SWARM_TERM_UPDATE_LOG;
const UPDATE_TAIL_BYTES: usize = 4096;

#[derive(Debug, Clone, Serialize, Type)]
#[serde(rename_all = "camelCase")]
struct UpdateLogLine {
    stream: &'static str,
    line: String,
}

fn tail_keep(buf: &str, limit: usize) -> String {
    if buf.len() <= limit {
        return buf.to_string();
    }
    let start = buf.len() - limit;
    // Snap to a char boundary so a multibyte glyph doesn't get cut.
    let mut i = start;
    while i < buf.len() && !buf.is_char_boundary(i) {
        i += 1;
    }
    buf[i..].to_string()
}

/// Update the host's `claude` CLI to the latest version. Rejects while a
/// swarm-term session is running so the binary on disk isn't swapped
/// out from under the spawned REPLs.
///
/// Resolution strategy:
/// 1. Resolve the active claude binary via `resolve_claude_spawn()`.
/// 2. If the resolved path looks like an npm install (under `\npm\` or
///    `node_modules\@anthropic-ai\claude-code`), run
///    `npm install -g @anthropic-ai/claude-code@latest`.
/// 3. Otherwise, invoke `claude update` directly — the v2.x native
///    installer supports this subcommand.
///
/// Stdout / stderr are streamed line-by-line as `swarm-term:update:log`
/// events. The final result carries the exit code plus the last 4KB of
/// each stream for a post-mortem display.
/// In-flight guard for [`swarm_term_run_update`]: two concurrent
/// `npm install -g` runs corrupt the global claude install. The
/// frontend disables the button on `isPending`, but a hung updater
/// keeps the mutation pending across remounts — the backend is the
/// authoritative gate.
static UPDATE_IN_FLIGHT: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Ceiling on the updater subprocess. npm installs finish in well
/// under a minute on a healthy network; 10 min only catches a wedged
/// npm/registry hang so the in-flight guard can't stay latched forever.
const UPDATE_WAIT_BUDGET: Duration = Duration::from_secs(10 * 60);

struct UpdateGuard;

impl Drop for UpdateGuard {
    fn drop(&mut self) {
        UPDATE_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tauri::command]
#[specta::specta]
pub async fn swarm_term_run_update<R: Runtime>(
    app: AppHandle<R>,
) -> Result<ClaudeUpdateResult, AppError> {
    if UPDATE_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err(AppError::Conflict(
            "a claude update is already in progress".into(),
        ));
    }
    let _guard = UpdateGuard;

    // Gate: refuse while a session is live — replacing the binary
    // mid-flight is exactly the failure mode the button exists to avoid.
    {
        let registry = app
            .state::<std::sync::Arc<crate::swarm_term::TerminalSwarmRegistry>>()
            .inner()
            .clone();
        if registry.current().is_some() {
            return Err(AppError::Conflict(
                "cannot update claude while a swarm session is running; \
                 stop it first"
                    .into(),
            ));
        }
    }

    let spawn = resolve_claude_spawn()?;
    let claude_path_lower = spawn.program.to_string_lossy().to_lowercase();
    let is_npm_install = claude_path_lower.contains("\\npm\\")
        || claude_path_lower.contains("/npm/")
        || claude_path_lower
            .contains("node_modules\\@anthropic-ai\\claude-code")
        || claude_path_lower
            .contains("node_modules/@anthropic-ai/claude-code");

    let env = subscription_env();

    let mut cmd = if is_npm_install {
        // npm-installed: shell out to npm. On Windows the binary is
        // `npm.cmd`; rely on PATH resolution.
        let npm = if cfg!(target_os = "windows") { "npm.cmd" } else { "npm" };
        let mut c = Command::new(npm);
        c.args([
            "install",
            "-g",
            "@anthropic-ai/claude-code@latest",
        ]);
        c
    } else {
        // Native install: claude's own update subcommand.
        let mut c = Command::new(&spawn.program);
        for a in &spawn.prefix_args {
            c.arg(a);
        }
        c.arg("update");
        c
    };

    cmd.envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Internal(format!("spawn claude updater: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Internal("updater stdout pipe missing".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Internal("updater stderr pipe missing".into()))?;

    let app_out = app.clone();
    let stdout_task = tokio::spawn(async move {
        let mut buf = String::new();
        let mut reader = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let _ = app_out.emit(
                UPDATE_LOG_EVENT,
                UpdateLogLine { stream: "stdout", line: line.clone() },
            );
            buf.push_str(&line);
            buf.push('\n');
        }
        buf
    });

    let app_err = app.clone();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let _ = app_err.emit(
                UPDATE_LOG_EVENT,
                UpdateLogLine { stream: "stderr", line: line.clone() },
            );
            buf.push_str(&line);
            buf.push('\n');
        }
        buf
    });

    let status = match tokio::time::timeout(UPDATE_WAIT_BUDGET, child.wait())
        .await
    {
        Ok(res) => res
            .map_err(|e| AppError::Internal(format!("wait updater: {e}")))?,
        Err(_) => {
            // Kill the wedged updater so the next attempt starts clean
            // (kill_on_drop is belt-and-suspenders for the kill failing).
            let _ = child.kill().await;
            return Err(AppError::Timeout(format!(
                "claude updater did not finish within {}s",
                UPDATE_WAIT_BUDGET.as_secs()
            )));
        }
    };

    let stdout_full = stdout_task.await.unwrap_or_default();
    let stderr_full = stderr_task.await.unwrap_or_default();

    Ok(ClaudeUpdateResult {
        exit_code: status.code().unwrap_or(-1),
        stdout_tail: tail_keep(&stdout_full, UPDATE_TAIL_BYTES),
        stderr_tail: tail_keep(&stderr_full, UPDATE_TAIL_BYTES),
    })
}

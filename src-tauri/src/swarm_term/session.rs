//! `TerminalSwarmSession` lifecycle.
//!
//! `start()` spawns 9 PTY panes (one per agent in `hierarchy::AGENT_IDS`)
//! through `TerminalRegistry::spawn_pane`, each running interactive
//! `claude` with the user-selected project as `cwd`. Before the spawn
//! loop it prepares `<project>/.bridgespace/<session>/{inbox,processed,
//! rejected}/<agent>/` and passes per-pane environment variables
//! (`NEURON_BRIDGE`, `NEURON_AGENT_ID`, `NEURON_INBOX`) so each claude
//! REPL knows where to drop outbound messages. The
//! [`bridge::watcher_loop`] picks up the JSON envelopes those drops
//! create and delivers their body into the appropriate target pane via
//! bracketed paste.
//!
//! `stop()` kills every pane, uninstalls the bridge watcher, and
//! deletes the `.bridgespace/<session>/` tree along with the per-pane
//! HOME isolation directories.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager, Runtime};
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::PaneSpawnInput;
use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm::binding::resolve_claude_spawn;
use crate::swarm::profile::ProfileRegistry;
use crate::swarm_term::bridge::{
    self, BridgeHandle, BRIDGE_DIRNAME,
};
use crate::swarm_term::hierarchy::AGENT_IDS;
use crate::swarm_term::home_isolation::{
    prepare_isolated_homes_root, seed_pane_home,
};
use crate::swarm_term::lifecycle::LifecycleStore;
use crate::swarm_term::persona::build_persona_payload;

const READY_DELAY_MS: u64 = 1500;

/// Delay after persona injection before the auto-prompt
/// (`NEURON_TERM_AUTO_PROMPT`) is pasted into the orchestrator pane.
/// Claude's bracketed-paste render of the persona body + footer +
/// first `@<id> hazır.` response settles in 5–10 s for short
/// personas; 10 s is safe headroom. Only consulted when the env
/// var is set, so production paths pay zero cost.
const AUTO_PROMPT_DELAY_MS: u64 = 10_000;

/// Gap between consecutive `claude.exe` PTY spawns. claude writes
/// `~/.claude.json` (startup counter, tipsHistory, lastPlanModeUse
/// etc.) within the first ~300 ms of process boot. Without a gap,
/// 9 parallel-ish spawns all open the file in write mode, race,
/// and leave it as concatenated `...}\n}\n}` — invalid JSON. On
/// next launch claude refuses to start and shows
/// `Configuration Error: invalid JSON`. 500 ms is generous against
/// observed first-write timing and imperceptible against the 11–13 s
/// persona-injection budget.
const SPAWN_STAGGER_MS: u64 = 500;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSwarmSessionHandle {
    pub session_id: String,
    pub project_dir: String,
    pub panes: Vec<TerminalSwarmPane>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSwarmPane {
    pub agent_id: String,
    pub pane_id: String,
}

#[derive(Default)]
pub struct TerminalSwarmRegistry {
    inner: Mutex<Option<ActiveSession>>,
}

pub(crate) struct ActiveSession {
    pub session_id: String,
    pub project_dir: PathBuf,
    pub panes_by_agent: HashMap<String, String>,
    /// Bridge watcher handle. Holding it keeps the watcher task alive;
    /// `bridge::uninstall` in `stop()` cancels the task. Optional only
    /// to keep the construction order flexible — once the session is
    /// inserted into the registry the field is always `Some`.
    pub bridge: Option<BridgeHandle>,
    /// Per-session `.bridgespace/<session>/` root. Cleaned up in
    /// `stop()` along with `homes_root`.
    pub bridgespace_root: Option<PathBuf>,
    /// Per-session HOME isolation root (under
    /// `app_data_dir/swarm-term/homes/<session_id>`). Cleaned up
    /// in `stop()`. `None` for synthetic / test sessions that
    /// bypass the spawn loop.
    pub homes_root: Option<PathBuf>,
    /// Pane ids that have completed persona injection and are safe
    /// targets for inter-agent routes. Populated by the persona-inject
    /// async task as each `write_to_pane` call returns Ok; read by
    /// `bridge::process_one` to short-circuit routes destined for
    /// panes that are still warming up (the claude REPL may not have
    /// enabled bracketed-paste mode yet, in which case the routed
    /// body is dumped into the prompt as raw text and submitted with
    /// the first `\r`, corrupting the receiver's first dispatch).
    ///
    /// `#[allow(dead_code)]`: the field's job is to KEEP THE Arc
    /// ALIVE for the session's lifetime. The actual reader is the
    /// bridge (via the clone we hand to `bridge::install`); this
    /// field is the session-side anchor preventing the Arc from
    /// dropping if `start` returns without the bridge holding it.
    /// No external code reads this field directly.
    #[allow(dead_code)]
    pub ready_panes: Arc<Mutex<HashSet<String>>>,
}

impl TerminalSwarmRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn current(&self) -> Option<TerminalSwarmSessionHandle> {
        let guard = self.inner.lock().ok()?;
        let s = guard.as_ref()?;
        Some(handle_from(s))
    }

    /// Spawn 9 PTY panes (one per agent in `AGENT_IDS`) and register
    /// them as the active session. Rejects if a session is already
    /// running — callers should `stop()` first.
    pub async fn start<R: Runtime>(
        self: &Arc<Self>,
        app: AppHandle<R>,
        project_dir: PathBuf,
    ) -> Result<TerminalSwarmSessionHandle, AppError> {
        // PATH-01: reject parent-dir traversal segments so a crafted
        // project_dir cannot walk out of wherever the caller intended
        // (the 9 REPLs spawn with bypassPermissions and cwd = this dir).
        if project_dir
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(AppError::InvalidInput(
                "project_dir must not contain '..' path segments".into(),
            ));
        }
        if !project_dir.is_dir() {
            return Err(AppError::InvalidInput(format!(
                "project_dir {} does not exist or is not a directory",
                project_dir.display()
            )));
        }
        {
            let guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            if guard.is_some() {
                return Err(AppError::Conflict(
                    "a terminal-swarm session is already running; stop it first"
                        .into(),
                ));
            }
        }

        let spawn = resolve_claude_spawn()?;
        let project_str = project_dir.display().to_string();
        let mut parts: Vec<String> =
            vec![format!("\"{}\"", spawn.program.display())];
        for a in &spawn.prefix_args {
            parts.push(format!("\"{}\"", a));
        }
        parts.push("--permission-mode".to_string());
        parts.push("bypassPermissions".to_string());
        let _ = project_str.clone(); // kept for log diagnostics
        let cmd = parts.join(" ");
        tracing::info!(
            project_dir = %project_str,
            cmd = %cmd,
            "swarm-term: spawning 9 claude REPLs"
        );

        let registry =
            app.state::<TerminalRegistry>().inner().clone();
        let pool = app.state::<DbPool>().inner().clone();

        // ---- Bridge filesystem layout ------------------------------- //
        // The per-session bridge root lives under the user's project
        // root so the agents' Write tool can target it with a path
        // that's natural in the cwd. A `.gitignore` at the parent
        // keeps per-session message files out of `git status`.
        let session_id = format!("swarm-term-{}", Ulid::new());
        let bridge_parent = project_dir.join(BRIDGE_DIRNAME);
        std::fs::create_dir_all(&bridge_parent).map_err(|e| {
            AppError::Internal(format!(
                "mkdir {}: {e}",
                bridge_parent.display()
            ))
        })?;
        let _ = bridge::ensure_gitignore(&bridge_parent);
        let bridgespace_root = bridge_parent.join(&session_id);
        bridge::prepare_layout(&bridgespace_root)?;

        // Per-session HOME isolation root. Each pane gets a private
        // subdir with copies of `~/.claude.json` + `~/.claude/`
        // top-level files so the 9 claude.exe processes don't race on
        // the user's real `~/.claude.json` and truncate it.
        let homes_root = prepare_isolated_homes_root(&app, &session_id)?;

        let mut panes_by_agent: HashMap<String, String> = HashMap::new();
        let mut spawned: Vec<String> = Vec::new();
        for (idx, &agent_id) in AGENT_IDS.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(SPAWN_STAGGER_MS)).await;
            }
            let pane_home = match seed_pane_home(&homes_root, agent_id) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(
                        agent_id = %agent_id,
                        error = %e,
                        "swarm-term: failed to seed pane HOME — falling back to shared HOME (corruption risk)"
                    );
                    for pid in &spawned {
                        let _ = registry.kill_pane(pid, &pool).await;
                    }
                    let _ = std::fs::remove_dir_all(&homes_root);
                    let _ = std::fs::remove_dir_all(&bridgespace_root);
                    return Err(AppError::Internal(format!(
                        "seed_pane_home({agent_id}): {e}"
                    )));
                }
            };
            let mut extra_env: HashMap<String, String> = HashMap::new();
            let pane_home_str = pane_home.display().to_string();
            extra_env.insert("HOME".to_string(), pane_home_str.clone());
            extra_env.insert("USERPROFILE".to_string(), pane_home_str);
            // Suppress claude CLI's in-process auto-updater. A mid-session
            // update exits the REPL, severing the PTY and forcing the user
            // to restart the whole app (losing bridgespace + lifecycle).
            // Updates are now user-triggered via the "Update Claude"
            // button which only fires when no session is active. Two
            // names are set because the flag was renamed between minor
            // versions — both are cheap and only one needs to land.
            extra_env.insert("DISABLE_AUTOUPDATER".to_string(), "1".to_string());
            extra_env.insert("CLAUDE_CODE_DISABLE_AUTOUPDATE".to_string(), "1".to_string());
            // ENV-01: belt-and-suspenders auth isolation. The registry
            // already env_remove's these for claude-code panes, but if
            // Neuron itself was launched from a Claude shell the parent's
            // token could bleed in via inheritance order. Forcing them
            // empty guarantees each pane authenticates only from its own
            // seeded ~/.claude/.credentials.json (the isolated HOME).
            extra_env.insert("CLAUDE_CODE_OAUTH_TOKEN".to_string(), String::new());
            extra_env.insert("ANTHROPIC_API_KEY".to_string(), String::new());
            // Bridge-related env vars. Each pane's claude REPL reads
            // these from the persona body (interpolated by
            // `build_persona_payload`), so they're informational here
            // — but exporting them lets the user `echo $NEURON_BRIDGE`
            // in the pane to verify the path during debugging.
            let bridge_root_str = bridgespace_root.display().to_string();
            let inbox_self = bridgespace_root
                .join("inbox")
                .join(agent_id)
                .display()
                .to_string();
            extra_env.insert(
                "NEURON_BRIDGE".to_string(),
                bridge_root_str.clone(),
            );
            extra_env.insert(
                "NEURON_AGENT_ID".to_string(),
                agent_id.to_string(),
            );
            extra_env.insert("NEURON_INBOX".to_string(), inbox_self);
            let input = PaneSpawnInput {
                cwd: project_str.clone(),
                cmd: Some(cmd.clone()),
                cols: Some(400),
                rows: Some(30),
                agent_kind: Some("claude-code".into()),
                role: Some(agent_id.to_string()),
                workspace: Some("swarm-term".into()),
                extra_env: Some(extra_env),
            };
            match registry
                .clone()
                .spawn_pane(input, app.clone(), pool.clone())
                .await
            {
                Ok(pane) => {
                    panes_by_agent.insert(agent_id.to_string(), pane.id.clone());
                    spawned.push(pane.id);
                }
                Err(e) => {
                    for pid in &spawned {
                        let _ = registry.kill_pane(pid, &pool).await;
                    }
                    let _ = std::fs::remove_dir_all(&homes_root);
                    let _ = std::fs::remove_dir_all(&bridgespace_root);
                    return Err(AppError::Internal(format!(
                        "spawn agent `{agent_id}`: {e}"
                    )));
                }
            }
        }

        // Per-session "ready" tracker — populated as each pane's
        // persona injection write completes. Routes to a not-yet-ready
        // pane short-circuit with `target_not_ready` in
        // `bridge::process_one` so the pre-bracketed-paste-mode window
        // doesn't corrupt the first inter-agent dispatch.
        let ready_panes: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));
        // Per-session lifecycle store — drives the autonomy contract
        // (Builder→Reviewer / Reviewer→Orchestrator fanouts). Owned by
        // the session and read by the bridge watcher via the Arc clone
        // handed to `bridge::install`.
        let lifecycle = Arc::new(LifecycleStore::new());

        // Install the bridge watcher BEFORE inserting the session
        // — once it's live, any envelope dropped into `inbox/` fires
        // a delivery. Persona injection (below) is what kicks the chain
        // off, so the watcher needs to be ready first.
        let bridge_handle = bridge::install(
            app.clone(),
            bridgespace_root.clone(),
            panes_by_agent.clone(),
            Arc::clone(&ready_panes),
            Arc::clone(&lifecycle),
        );

        let session = ActiveSession {
            session_id,
            project_dir: project_dir.clone(),
            panes_by_agent: panes_by_agent.clone(),
            bridge: Some(bridge_handle),
            bridgespace_root: Some(bridgespace_root.clone()),
            homes_root: Some(homes_root),
            ready_panes: Arc::clone(&ready_panes),
        };
        let handle = handle_from(&session);
        {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            *guard = Some(session);
        }

        // Fire-and-forget persona injection: wait for the claude REPLs
        // to settle, then paste each persona body + bridge-protocol
        // section into its pane.
        let app_for_inject = app.clone();
        let registry_for_inject = registry.clone();
        let ready_panes_for_inject = Arc::clone(&ready_panes);
        let bridge_root_for_inject = bridgespace_root.clone();
        let workspace_agents_dir = app
            .path()
            .app_data_dir()
            .ok()
            .map(|p| p.join("swarm-term").join("agents"))
            .filter(|p| p.is_dir());
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(READY_DELAY_MS)).await;
            let profiles =
                match ProfileRegistry::load_term(workspace_agents_dir.as_deref()) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "swarm-term: failed to load profiles for injection"
                        );
                        return;
                    }
                };
            for (agent_id, pane_id) in &panes_by_agent {
                let Some(profile) = profiles.get(agent_id) else {
                    tracing::warn!(
                        agent_id = %agent_id,
                        "swarm-term: no profile found, skipping injection"
                    );
                    continue;
                };
                let body = build_persona_payload(
                    agent_id,
                    &profile.body,
                    &bridge_root_for_inject,
                );
                match registry_for_inject
                    .write_to_pane(pane_id, body.as_bytes())
                    .await
                {
                    Ok(()) => {
                        if let Ok(mut g) = ready_panes_for_inject.lock() {
                            g.insert(pane_id.clone());
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            agent_id = %agent_id,
                            error = %e,
                            "swarm-term: persona injection write failed"
                        );
                    }
                }
            }

            if let Ok(auto_prompt) = std::env::var("NEURON_TERM_AUTO_PROMPT") {
                let trimmed = auto_prompt.trim();
                if !trimmed.is_empty() {
                    tokio::time::sleep(Duration::from_millis(AUTO_PROMPT_DELAY_MS)).await;
                    if let Some(orch_pane) = panes_by_agent.get("orchestrator") {
                        let payload = format!(
                            "\x1b[200~{trimmed}\x1b[201~\r"
                        );
                        if let Err(e) = registry_for_inject
                            .write_to_pane(orch_pane, payload.as_bytes())
                            .await
                        {
                            tracing::warn!(
                                error = %e,
                                "swarm-term: auto-prompt write failed"
                            );
                        } else {
                            tracing::info!(
                                prompt_len = trimmed.len(),
                                "swarm-term: auto-prompt injected into orchestrator"
                            );
                        }
                    } else {
                        tracing::warn!(
                            "swarm-term: auto-prompt set but orchestrator pane not found"
                        );
                    }
                }
            }
            drop(app_for_inject);
        });

        Ok(handle)
    }

    /// Kill every pane in the active session and clear the slot.
    /// Idempotent — calling on an empty registry is a no-op.
    pub async fn stop<R: Runtime>(
        &self,
        app: AppHandle<R>,
    ) -> Result<(), AppError> {
        let (pane_ids, bridge, homes_root, bridgespace_root): (
            Vec<String>,
            Option<BridgeHandle>,
            Option<PathBuf>,
            Option<PathBuf>,
        ) = {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            match guard.take() {
                Some(s) => (
                    s.panes_by_agent.into_values().collect(),
                    s.bridge,
                    s.homes_root,
                    s.bridgespace_root,
                ),
                None => return Ok(()),
            }
        };
        // Stop the bridge watcher BEFORE killing panes so it doesn't
        // race-deliver during shutdown.
        if let Some(b) = bridge {
            bridge::uninstall(b);
        }
        let registry = app.state::<TerminalRegistry>().inner().clone();
        let pool = app.state::<DbPool>().inner().clone();
        for pid in pane_ids {
            let _ = registry.kill_pane(&pid, &pool).await;
        }
        if let Some(root) = homes_root {
            if let Err(e) = std::fs::remove_dir_all(&root) {
                tracing::warn!(
                    path = %root.display(),
                    error = %e,
                    "swarm-term: isolated homes cleanup failed"
                );
            } else {
                tracing::info!(
                    path = %root.display(),
                    "swarm-term: isolated homes cleaned up"
                );
            }
        }
        if let Some(root) = bridgespace_root {
            if let Err(e) = std::fs::remove_dir_all(&root) {
                tracing::warn!(
                    path = %root.display(),
                    error = %e,
                    "swarm-term: bridgespace cleanup failed"
                );
            } else {
                tracing::info!(
                    path = %root.display(),
                    "swarm-term: bridgespace cleaned up"
                );
            }
        }
        Ok(())
    }
}

fn handle_from(s: &ActiveSession) -> TerminalSwarmSessionHandle {
    let mut panes: Vec<TerminalSwarmPane> = s
        .panes_by_agent
        .iter()
        .map(|(agent_id, pane_id)| TerminalSwarmPane {
            agent_id: agent_id.clone(),
            pane_id: pane_id.clone(),
        })
        .collect();
    panes.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    TerminalSwarmSessionHandle {
        session_id: s.session_id.clone(),
        project_dir: s.project_dir.display().to_string(),
        panes,
    }
}


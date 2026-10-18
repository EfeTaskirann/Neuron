//! `TerminalSwarmSession` lifecycle.
//!
//! `start()` spawns 9 PTY panes (one per agent in `hierarchy::AGENT_IDS`)
//! through `TerminalRegistry::spawn_pane`, each running interactive
//! `claude` with the user-selected project as `cwd`. Before the spawn
//! loop it prepares `<project>/.bridgespace/<session>/{outbox,processed,
//! rejected}/<agent>/` and passes per-pane environment variables
//! (`NEURON_BRIDGE`, `NEURON_AGENT_ID`, `NEURON_OUTBOX`) so each claude
//! REPL knows where to drop outbound messages. Each pane's persona +
//! bridge protocol is its appended system prompt
//! (`--append-system-prompt-file`); a short kickoff turn is then pasted
//! and the pane counts as ready once claude answers `@<id> hazır.`. The
//! [`bridge::watcher::watcher_loop`] picks up the JSON envelopes those drops
//! create and delivers their body into the appropriate target pane via
//! bracketed paste.
//!
//! `stop()` kills every pane, uninstalls the bridge watcher, archives
//! `rejected/`, and deletes the `.bridgespace/<session>/` tree along
//! with the per-pane HOME isolation directories.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use crate::swarm_term::heartbeat;
use crate::swarm_term::hierarchy::AGENT_IDS;
use crate::swarm_term::home_isolation::{
    ensure_project_trust, prepare_isolated_homes_root, seed_pane_home,
    sweep_orphan_homes, sync_credentials,
};
use crate::swarm_term::lifecycle::LifecycleStore;
use crate::swarm_term::permissions::{self, SwarmTermPermissionMode};
use crate::swarm_term::project_guard;
use crate::swarm_term::persona::{
    build_kickoff_message, build_persona_prompt, is_ready_answer,
};

const READY_DELAY_MS: u64 = 1500;

/// File (inside each pane's isolated HOME) holding the persona + bridge
/// protocol, passed to claude with `--append-system-prompt-file`.
const PERSONA_FILENAME: &str = "neuron-persona.md";

/// How long to wait for a pane's `@<id> hazır.` answer after its
/// kickoff was SENT before giving up on it (logged; the pane stays
/// not-ready so the bridge returns messages addressed to it to their
/// senders).
const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// How long a pane on a login / trust / first-run screen is watched for
/// the user to resolve it; its kickoff goes out as soon as the screen
/// clears. (It used to be given up with the rest after 90 s, so a screen
/// answered later left the pane not-ready for the whole session.)
const BLOCKED_SCREEN_WAIT: Duration = Duration::from_secs(30 * 60);

/// How often a pane whose kickoff is not sent yet is re-checked.
const BLOCKED_SCREEN_POLL: Duration = Duration::from_secs(1);

/// A pane's kickoff goes out once its screen is non-empty, not a
/// blocking screen, and unchanged for this many consecutive checks.
/// claude 2.1.29x takes up to ~40 s to boot and may then open the
/// folder-trust screen; a kickoff pasted into the still-blank pane was
/// buffered, and its Enter answered the trust screen's default
/// "No, exit" — the pane's claude quit before anyone saw the screen.
const SETTLED_CHECKS: u32 = 2;

/// Fallback: a pane whose screen never settles (an animation) gets its
/// kickoff anyway after this long — still never onto a blocking screen.
const SETTLE_FALLBACK: Duration = Duration::from_secs(60);

/// If a pane has not answered by then, the kickoff is pasted once more
/// (it may have landed before the REPL finished booting).
const KICKOFF_RESEND_AFTER: Duration = Duration::from_secs(30);

const READY_POLL: Duration = Duration::from_millis(500);

/// Cadence of the in-session credentials sync (see
/// [`sync_credentials`]). A refresh happens at most every few hours, so
/// a minute is ample to spread it before another pane needs it.
const CREDENTIALS_SYNC_EVERY: Duration = Duration::from_secs(60);

/// `remove_dir_all` on the homes root right after killing the panes can
/// fail with "file in use" (os error 32) while the claude processes are
/// still exiting; retry this many times, [`CLEANUP_RETRY_DELAY`] apart.
const CLEANUP_ATTEMPTS: u32 = 10;
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Delay after the readiness wait before the auto-prompt
/// (`NEURON_TERM_AUTO_PROMPT`) is pasted into the orchestrator pane.
/// Only consulted when the env var is set, so production paths pay
/// zero cost.
const AUTO_PROMPT_DELAY_MS: u64 = 2_000;

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
    /// What the panes may do — the mode they were spawned with.
    pub permission_mode: SwarmTermPermissionMode,
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
    /// TOCTOU guard for `start()`: the busy-check and the final slot
    /// insert are separated by a multi-second await window (9 staggered
    /// pane spawns). A second concurrent `start` passing the `is_some()`
    /// check in that window would overwrite the first session at insert
    /// time, leaking its 9 PTY panes and bridge watcher. Set under the
    /// same lock as the busy-check; cleared by [`StartReservation`] on
    /// every exit path.
    starting: std::sync::atomic::AtomicBool,
}

/// RAII clear for [`TerminalSwarmRegistry::starting`] — dropping it
/// releases the reservation on success and on every early-error return
/// of `start()` alike.
struct StartReservation<'a>(&'a TerminalSwarmRegistry);

impl Drop for StartReservation<'_> {
    fn drop(&mut self) {
        self.0
            .starting
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

pub(crate) struct ActiveSession {
    pub session_id: String,
    pub project_dir: PathBuf,
    pub panes_by_agent: HashMap<String, String>,
    /// Permission mode every pane of this session was spawned with.
    pub permission_mode: SwarmTermPermissionMode,
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
    /// `bridge::watcher::process_one` to short-circuit routes destined for
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
    /// Cleared by `stop()`. The session's background tasks (kickoff /
    /// readiness wait, credentials sync) poll it and exit, so a stopped
    /// session's tasks never act on the panes of the next one.
    pub alive: Arc<AtomicBool>,
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
        // (the 9 REPLs spawn with cwd = this dir and may read — builders
        // also write — everything under it; see `permissions`).
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
        // The project folder is every agent's reach: refuse drive roots,
        // the home folder, app-data, credential and OS folders.
        project_guard::check(&app, &project_dir)?;
        let _reservation = {
            let guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            if guard.is_some() {
                return Err(AppError::Conflict(
                    "a terminal-swarm session is already running; stop it first"
                        .into(),
                ));
            }
            // Reserve the slot while still under the lock — the spawn
            // sequence below awaits for seconds, and a second start()
            // racing through this window would otherwise overwrite the
            // session at the final insert.
            if self
                .starting
                .swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(AppError::Conflict(
                    "a terminal-swarm session is already starting".into(),
                ));
            }
            StartReservation(self)
        };

        let spawn = resolve_claude_spawn()?;
        let project_str = project_dir.display().to_string();
        let mut parts: Vec<String> =
            vec![format!("\"{}\"", spawn.program.display())];
        for a in &spawn.prefix_args {
            parts.push(format!("\"{}\"", a));
        }
        // Permission flags are per agent (see `permissions`), appended in
        // the spawn loop.
        let _ = project_str.clone(); // kept for log diagnostics
        let cmd = parts.join(" ");
        tracing::info!(
            project_dir = %project_str,
            cmd = %cmd,
            "swarm-term: spawning 9 claude REPLs"
        );
        // Read once: the same mode drives every pane's flags and is
        // what the session reports to the UI.
        let permission_mode = permissions::current_mode();
        if permission_mode == SwarmTermPermissionMode::Bypass {
            tracing::warn!(
                "swarm-term: {}=bypass — all panes run with bypassPermissions \
                 (unrestricted shell + file access)",
                permissions::PERMISSIONS_ENV
            );
        }
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent();
        if let Some(warning) = source_root.and_then(|root| {
            self_hosting_warning(&project_dir, root, cfg!(debug_assertions))
        }) {
            tracing::warn!("swarm-term: {warning}");
            let _ = tauri::Emitter::emit(
                &app,
                crate::events::SWARM_TERM_ROUTE,
                serde_json::json!({
                    "source": "neuron",
                    "target": "orchestrator",
                    "body": format!("[Neuron] {warning}"),
                    "outcome": "notice",
                    "reason": "swarm runs on this app's own source tree",
                }),
            );
        }

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
        // No session is running (the reservation above guarantees it),
        // so every leftover session tree / pane-HOME is an orphan of a
        // session that never reached `stop()`. Sweep them BEFORE seeding
        // the new panes: the homes sweep also syncs a possibly fresher,
        // rotated token back into the user's real credentials file,
        // which the new panes then copy.
        let orphan_sessions = bridge::sweep_orphan_sessions(&bridge_parent);
        let orphan_homes = sweep_orphan_homes(&app);
        if orphan_sessions + orphan_homes > 0 {
            tracing::info!(
                orphan_sessions,
                orphan_homes,
                "swarm-term: swept leftovers of sessions that never stopped"
            );
        }
        let bridgespace_root = bridge_parent.join(&session_id);
        bridge::prepare_layout(&bridgespace_root)?;
        heartbeat::beat(&bridgespace_root);

        // Per-session HOME isolation root. Each pane gets a private
        // subdir with copies of `~/.claude.json` + `~/.claude/`
        // top-level files so the 9 claude.exe processes don't race on
        // the user's real `~/.claude.json` and truncate it.
        let homes_root = prepare_isolated_homes_root(&app, &session_id)?;
        heartbeat::beat(&homes_root);

        // Personas are loaded BEFORE spawning: each pane gets its
        // persona + bridge protocol as an appended SYSTEM prompt
        // (`--append-system-prompt-file`), so it must exist on disk
        // when claude starts.
        let workspace_agents_dir = app
            .path()
            .app_data_dir()
            .ok()
            .map(|p| p.join("swarm-term").join("agents"))
            .filter(|p| p.is_dir());
        let profiles =
            match ProfileRegistry::load_term(workspace_agents_dir.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&homes_root);
                    let _ = std::fs::remove_dir_all(&bridgespace_root);
                    return Err(AppError::Internal(format!(
                        "load swarm-term personas: {e}"
                    )));
                }
            };

        let mut panes_by_agent: HashMap<String, String> = HashMap::new();
        let mut pane_homes: HashMap<String, PathBuf> = HashMap::new();
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
            // Settle the folder-trust screen for this pane (see
            // `ensure_project_trust`).
            let trust = ensure_project_trust(&pane_home, &project_dir);
            tracing::debug!(agent_id = %agent_id, trust = ?trust, "swarm-term: pane folder trust");
            // Persona + bridge protocol → system-prompt file inside the
            // pane's private HOME (deleted with it on stop).
            let persona_file = pane_home.join(PERSONA_FILENAME);
            let persona_written = profiles
                .get(agent_id)
                .ok_or_else(|| format!("no persona profile for `{agent_id}`"))
                .and_then(|profile| {
                    let prompt = build_persona_prompt(
                        agent_id,
                        &profile.body,
                        &bridgespace_root,
                    );
                    #[cfg(test)]
                    let prompt = tests::with_injection(agent_id, prompt);
                    std::fs::write(&persona_file, prompt)
                        .map_err(|e| format!("write {}: {e}", persona_file.display()))
                });
            if let Err(e) = persona_written {
                for pid in &spawned {
                    let _ = registry.kill_pane(pid, &pool).await;
                }
                let _ = std::fs::remove_dir_all(&homes_root);
                let _ = std::fs::remove_dir_all(&bridgespace_root);
                return Err(AppError::Internal(format!(
                    "swarm-term persona for `{agent_id}`: {e}"
                )));
            }
            let agent_cmd = format!(
                "{cmd} {} --append-system-prompt-file \"{}\"",
                permissions::join_quoted(&permissions::claude_permission_args(
                    permission_mode,
                    agent_id,
                    &session_id,
                )),
                persona_file.display()
            );
            pane_homes.insert(agent_id.to_string(), pane_home.clone());
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
            let outbox_self = bridgespace_root
                .join("outbox")
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
            extra_env.insert("NEURON_OUTBOX".to_string(), outbox_self);
            let input = PaneSpawnInput {
                cwd: project_str.clone(),
                cmd: Some(agent_cmd),
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
        // `bridge::watcher::process_one` so the pre-bracketed-paste-mode window
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

        let alive = Arc::new(AtomicBool::new(true));
        let session = ActiveSession {
            session_id,
            project_dir: project_dir.clone(),
            panes_by_agent: panes_by_agent.clone(),
            permission_mode,
            bridge: Some(bridge_handle),
            bridgespace_root: Some(bridgespace_root.clone()),
            homes_root: Some(homes_root.clone()),
            ready_panes: Arc::clone(&ready_panes),
            alive: Arc::clone(&alive),
        };
        let handle = handle_from(&session);
        {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            *guard = Some(session);
        }

        // Fire-and-forget kickoff: wait for the claude REPLs to settle,
        // paste the short kickoff turn into every pane, then mark each
        // pane ready only once claude has actually ANSWERED it with
        // `@<id> hazır.` (read from the pane's own transcript — the
        // terminal echo of the kickoff would be a false positive). A
        // pane stuck on a first-run screen therefore never becomes a
        // delivery target; messages to it are returned to their sender.
        let app_for_inject = app.clone();
        let registry_for_inject = registry.clone();
        let ready_panes_for_inject = Arc::clone(&ready_panes);
        let alive_for_inject = Arc::clone(&alive);
        let pool_for_inject = pool.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(READY_DELAY_MS)).await;
            if !alive_for_inject.load(Ordering::SeqCst) {
                return;
            }
            // No kickoff is pasted blind: the wait loop sends each one once
            // that pane's screen has settled (see `SETTLED_CHECKS`).
            let kicked: HashMap<String, Option<Instant>> =
                panes_by_agent.keys().map(|a| (a.clone(), None)).collect();
            wait_for_ready_answers(
                &registry_for_inject,
                &pool_for_inject,
                &panes_by_agent,
                &pane_homes,
                &ready_panes_for_inject,
                &alive_for_inject,
                kicked,
            )
            .await;
            if !alive_for_inject.load(Ordering::SeqCst) {
                return;
            }

            if let Ok(auto_prompt) = std::env::var("NEURON_TERM_AUTO_PROMPT") {
                let trimmed = auto_prompt.trim();
                if !trimmed.is_empty() {
                    tokio::time::sleep(Duration::from_millis(AUTO_PROMPT_DELAY_MS)).await;
                    if let Some(orch_pane) = panes_by_agent.get("orchestrator") {
                        let payload = format!(
                            "\x1b[200~{}\x1b[201~",
                            bridge::sanitize_for_paste(trimmed)
                        );
                        if let Err(e) = bridge::paste_then_submit(
                            &registry_for_inject,
                            orch_pane,
                            payload.as_bytes(),
                        )
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

        // Session upkeep, ends with the session:
        //   * heartbeat both on-disk trees so another Neuron instance's
        //     orphan sweep leaves them alone (see `heartbeat`);
        //   * credentials sync — spread a pane's token refresh to the
        //     other panes and back to the user's real file (see
        //     `home_isolation::sync_credentials`); `stop()` keeps syncing
        //     until the homes are gone.
        let alive_for_upkeep = Arc::clone(&alive);
        let bridge_root_for_upkeep = bridgespace_root.clone();
        tauri::async_runtime::spawn(async move {
            let mut since_sync = Duration::ZERO;
            loop {
                tokio::time::sleep(heartbeat::HEARTBEAT_EVERY).await;
                if !alive_for_upkeep.load(Ordering::SeqCst) {
                    return;
                }
                heartbeat::beat(&homes_root);
                heartbeat::beat(&bridge_root_for_upkeep);
                since_sync += heartbeat::HEARTBEAT_EVERY;
                if since_sync >= CREDENTIALS_SYNC_EVERY {
                    since_sync = Duration::ZERO;
                    sync_credentials(&homes_root);
                }
            }
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
                Some(s) => {
                    s.alive.store(false, Ordering::SeqCst);
                    (
                        s.panes_by_agent.into_values().collect(),
                        s.bridge,
                        s.homes_root,
                        s.bridgespace_root,
                    )
                }
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
            remove_homes_with_retry(&root).await;
        }
        if let Some(root) = bridgespace_root {
            // Keep the rejection evidence (envelopes + `.reason`) —
            // the rest of the session tree is disposable.
            match bridge::archive_rejected(&root) {
                Ok(Some(dest)) => tracing::info!(
                    path = %dest.display(),
                    "swarm-term: rejected messages archived"
                ),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    path = %root.display(),
                    error = %e,
                    "swarm-term: rejected-archive failed; deleting with session"
                ),
            }
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

/// Delete a stopped session's homes root, retrying while the just-killed
/// claude processes still hold files open (os error 32 on Windows).
///
/// Credentials are synced out of the homes before EVERY attempt: a pane
/// still exiting can write a refreshed (rotated) token after a single
/// up-front sync, and deleting the homes would then destroy the only
/// live refresh token — the next session would start in `/login`.
async fn remove_homes_with_retry(root: &Path) {
    for attempt in 1..=CLEANUP_ATTEMPTS {
        sync_credentials(root);
        match std::fs::remove_dir_all(root) {
            Ok(()) => {
                tracing::info!(
                    path = %root.display(),
                    attempt,
                    "swarm-term: isolated homes cleaned up"
                );
                return;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) if attempt == CLEANUP_ATTEMPTS => {
                tracing::warn!(
                    path = %root.display(),
                    error = %e,
                    attempts = CLEANUP_ATTEMPTS,
                    "swarm-term: isolated homes cleanup failed"
                );
            }
            Err(_) => tokio::time::sleep(CLEANUP_RETRY_DELAY).await,
        }
    }
}

/// Lower-case fragments of claude screens that are NOT the REPL prompt —
/// login method picker, first-run theme / trust / onboarding screens,
/// the expired-credentials banner. Pasting the kickoff + Enter there
/// would pick a menu option (e.g. start an OAuth browser flow, or — on
/// the trust screen of claude 2.1.29x — the default "No, exit", which
/// killed every pane) instead of reaching the model. Matched with all
/// whitespace removed: the pane renderer moves the cursor instead of
/// printing spaces, so the captured text reads `Quicksafetycheck:`.
const BLOCKING_SCREEN_MARKERS: &[&str] = &[
    "select login method",
    "choose the text style",
    "trust the files",
    "is this a project you created or one you trust",
    "yes, i trust this folder",
    "press enter to continue",
    "please run /login",
];

/// How many trailing pane lines are inspected for those markers.
const SCREEN_TAIL_LINES: usize = 60;

/// Pure: a warning when a DEV build runs the swarm on its own source
/// tree. Builders editing `src-tauri/` or `app/` there make `tauri dev`
/// rebuild and restart the app, which kills the running session.
fn self_hosting_warning(project: &Path, source_root: &Path, dev_build: bool) -> Option<String> {
    if !dev_build {
        return None;
    }
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Compared canonicalised, displayed as given (no `\\?\` prefix).
    canon(project).starts_with(canon(source_root)).then(|| {
        format!(
            "Swarm, dev modda çalışan bu uygulamanın kendi kaynak ağacında ({}) \
             çalışıyor: builder'ların `src-tauri/` / `app/` değişiklikleri `tauri dev`'i \
             yeniden başlatıp bu oturumu sonlandırır. Ayrı bir git worktree'de çalıştır \
             (ör. `git worktree add ../neuron-swarm`).",
            source_root.display()
        )
    })
}

fn screen_is_blocking(screen_tail: &str) -> bool {
    let squash = |s: &str| -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
    };
    let screen = squash(screen_tail);
    BLOCKING_SCREEN_MARKERS.iter().any(|m| screen.contains(&squash(m)))
}

/// The pane's recent output (last [`SCREEN_TAIL_LINES`] lines, oldest
/// first), or `None` when it cannot be read.
async fn pane_screen_tail(
    registry: &TerminalRegistry,
    pool: &DbPool,
    pane_id: &str,
) -> Option<String> {
    // Live panes: ring lines + the unflushed screen (a TUI paints with
    // cursor moves and few newlines). Closed panes: their stored lines.
    if let Some(text) = registry.pane_screen_text(pane_id, SCREEN_TAIL_LINES).await {
        return Some(text);
    }
    let lines = registry.pane_lines(pane_id, None, pool).await.ok()?;
    let mut tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(SCREEN_TAIL_LINES)
        .map(|l| l.text.as_str())
        .collect();
    tail.reverse();
    Some(tail.join("\n"))
}

/// True when the pane's recent output shows a login / first-run screen.
async fn pane_on_blocking_screen(
    registry: &TerminalRegistry,
    pool: &DbPool,
    pane_id: &str,
) -> bool {
    pane_screen_tail(registry, pool, pane_id)
        .await
        .is_some_and(|tail| screen_is_blocking(&tail))
}

/// Pure: per-pane "has the screen settled?" bookkeeping. Feed it each
/// check's screen tail; it says when the kickoff may go out — the screen
/// is non-empty, not blocking, and identical for [`SETTLED_CHECKS`]
/// checks in a row. A blocking screen resets it.
#[derive(Debug, Default)]
struct ScreenSettle {
    last: Option<String>,
    unchanged: u32,
}

impl ScreenSettle {
    fn observe(&mut self, screen: &str) -> bool {
        if screen.trim().is_empty() || screen_is_blocking(screen) {
            *self = Self::default();
            return false;
        }
        if self.last.as_deref() == Some(screen) {
            self.unchanged += 1;
        } else {
            self.last = Some(screen.to_string());
            self.unchanged = 1;
        }
        self.unchanged >= SETTLED_CHECKS
    }
}

/// Paste the kickoff turn into one pane (best-effort, logged) — unless
/// the pane is on a login / trust / first-run screen, where it is
/// withheld; `wait_for_ready_answers` sends it once the user resolves
/// the screen.
///
/// Returns whether the kickoff was pasted (`false` = withheld).
async fn send_kickoff(
    registry: &TerminalRegistry,
    pool: &DbPool,
    agent_id: &str,
    pane_id: &str,
) -> bool {
    if pane_on_blocking_screen(registry, pool, pane_id).await {
        tracing::warn!(
            agent_id = %agent_id,
            "swarm-term: kickoff withheld — pane is on a login / trust / \
             first-run screen; it is sent once the screen is resolved"
        );
        return false;
    }
    let kickoff = build_kickoff_message(agent_id);
    if let Err(e) =
        bridge::paste_then_submit(registry, pane_id, kickoff.as_bytes()).await
    {
        tracing::warn!(
            agent_id = %agent_id,
            error = %e,
            "swarm-term: kickoff write failed"
        );
    }
    true
}

/// Send each pane's kickoff and poll its claude transcript until it has
/// answered, marking it ready as soon as it does. `kicked` holds when
/// each pane's kickoff was sent (`None` = not yet).
///
/// * An unsent kickoff goes out once the pane's screen has settled on
///   something other than a login / trust / first-run screen
///   ([`ScreenSettle`]) — so it can never answer such a screen, and a
///   screen the user resolves late still gets its kickoff. Watched for
///   up to [`BLOCKED_SCREEN_WAIT`].
/// * A sent kickoff without an answer is re-sent once after
///   [`KICKOFF_RESEND_AFTER`] (it may have landed before the REPL
///   finished booting) and given up (warn, pane stays not-ready)
///   [`READY_TIMEOUT`] after it was sent.
///
/// Returns as soon as every pane is settled or the session is stopped
/// (`alive` cleared).
async fn wait_for_ready_answers(
    registry: &TerminalRegistry,
    pool: &DbPool,
    panes_by_agent: &HashMap<String, String>,
    pane_homes: &HashMap<String, PathBuf>,
    ready: &Arc<Mutex<HashSet<String>>>,
    alive: &AtomicBool,
    mut kicked: HashMap<String, Option<Instant>>,
) {
    let started = Instant::now();
    let mut pending: HashSet<&str> =
        panes_by_agent.keys().map(String::as_str).collect();
    let mut resent: HashSet<String> = HashSet::new();
    let mut settle: HashMap<String, ScreenSettle> = HashMap::new();
    let mut last_screen_check = Instant::now() - BLOCKED_SCREEN_POLL;
    loop {
        if !alive.load(Ordering::SeqCst) {
            return;
        }
        let answered: Vec<&str> = pending
            .iter()
            .copied()
            .filter(|agent| {
                pane_homes
                    .get(*agent)
                    .is_some_and(|home| transcript_has_ready_answer(home, agent))
            })
            .collect();
        for agent in answered {
            pending.remove(agent);
            if let (Some(pane), Ok(mut g)) = (panes_by_agent.get(agent), ready.lock()) {
                g.insert(pane.clone());
            }
            tracing::info!(
                agent_id = %agent,
                waited_ms = started.elapsed().as_millis() as u64,
                "swarm-term: pane answered kickoff — ready"
            );
        }

        let check_screens = last_screen_check.elapsed() >= BLOCKED_SCREEN_POLL;
        if check_screens {
            last_screen_check = Instant::now();
        }
        let mut gave_up: Vec<&str> = Vec::new();
        for &agent in &pending {
            let Some(pane) = panes_by_agent.get(agent) else {
                continue;
            };
            match kicked.get(agent).copied().flatten() {
                // Not sent yet: send it once the screen has settled on
                // something that is not a login / trust screen.
                None => {
                    if started.elapsed() >= BLOCKED_SCREEN_WAIT {
                        gave_up.push(agent);
                    } else if check_screens {
                        let screen = pane_screen_tail(registry, pool, pane).await.unwrap_or_default();
                        let settled = settle.entry(agent.to_string()).or_default().observe(&screen);
                        let fallback = started.elapsed() >= SETTLE_FALLBACK
                            && !screen.trim().is_empty()
                            && !screen_is_blocking(&screen);
                        if (settled || fallback) && send_kickoff(registry, pool, agent, pane).await {
                            tracing::info!(
                                agent_id = %agent,
                                waited_ms = started.elapsed().as_millis() as u64,
                                "swarm-term: pane screen settled — kickoff sent"
                            );
                            kicked.insert(agent.to_string(), Some(Instant::now()));
                        }
                    }
                }
                Some(sent_at) => {
                    let since = sent_at.elapsed();
                    if since >= READY_TIMEOUT {
                        gave_up.push(agent);
                    } else if since >= KICKOFF_RESEND_AFTER && resent.insert(agent.to_string()) {
                        tracing::warn!(
                            agent_id = %agent,
                            "swarm-term: no kickoff answer yet — re-sending once"
                        );
                        if !send_kickoff(registry, pool, agent, pane).await {
                            // A blocking screen appeared: wait for it again.
                            kicked.insert(agent.to_string(), None);
                        }
                    }
                }
            }
        }
        for agent in gave_up {
            pending.remove(agent);
            tracing::warn!(
                agent_id = %agent,
                "swarm-term: pane never answered the kickoff; left not-ready \
                 (messages to it are returned to the sender)"
            );
        }
        if pending.is_empty() {
            return;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// True when a claude transcript under `pane_home`
/// (`.claude/projects/<cwd-slug>/<session>.jsonl`) holds an assistant
/// turn that is `agent_id`'s readiness answer.
fn transcript_has_ready_answer(pane_home: &Path, agent_id: &str) -> bool {
    let Ok(project_dirs) = std::fs::read_dir(pane_home.join(".claude").join("projects"))
    else {
        return false;
    };
    project_dirs.flatten().any(|dir| {
        std::fs::read_dir(dir.path()).into_iter().flatten().flatten().any(|f| {
            let p = f.path();
            p.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && std::fs::read_to_string(&p).is_ok_and(|text| {
                    text.lines().any(|l| line_is_ready_answer(l, agent_id))
                })
        })
    })
}

fn line_is_ready_answer(line: &str, agent_id: &str) -> bool {
    use serde_json::Value;
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    match v.pointer("/message/content") {
        Some(Value::String(s)) => is_ready_answer(agent_id, s),
        Some(Value::Array(parts)) => parts.iter().any(|p| {
            p.get("type").and_then(Value::as_str) == Some("text")
                && p
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|t| is_ready_answer(agent_id, t))
        }),
        _ => false,
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
        permission_mode: s.permission_mode,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Live-harness fault injection: extra system-prompt text per agent
    /// (e.g. "reject every review"), so failure paths can be driven with
    /// REAL agents. Test builds only — the app never has it.
    static INJECTIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    pub(super) fn with_injection(agent_id: &str, prompt: String) -> String {
        let g = INJECTIONS.lock().unwrap_or_else(|p| p.into_inner());
        g.iter()
            .filter(|(agent, _)| agent == agent_id)
            .fold(prompt, |p, (_, extra)| {
                format!("{p}\n\n## TEST OTURUMU TALİMATI (geçerli, uygula)\n\n{extra}")
            })
    }

    const READY_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"@scout hazır."}]}}"#;
    const KICKOFF_ECHO: &str = r#"{"type":"user","message":{"role":"user","content":"[Neuron] Oturum başladı. @scout hazır."}}"#;

    #[test]
    fn a_dev_build_warns_when_the_swarm_edits_its_own_source() {
        let src = tempfile::tempdir().expect("tempdir");
        let inside = src.path().join("sub");
        std::fs::create_dir_all(&inside).expect("mkdir");
        let other = tempfile::tempdir().expect("tempdir");
        assert!(self_hosting_warning(src.path(), src.path(), true).is_some());
        assert!(self_hosting_warning(&inside, src.path(), true).is_some());
        assert!(self_hosting_warning(other.path(), src.path(), true).is_none());
        // A release build does not hot-reload — nothing to warn about.
        assert!(self_hosting_warning(src.path(), src.path(), false).is_none());
    }

    #[test]
    fn the_kickoff_waits_for_a_settled_non_blocking_screen() {
        let mut s = ScreenSettle::default();
        // Blank (still booting) never settles.
        assert!(!s.observe(""));
        assert!(!s.observe("   \n "));
        // A changing screen is not settled; the same screen twice is.
        assert!(!s.observe("✻ Welcome"));
        assert!(!s.observe("✻ Welcome\n❯ "));
        assert!(s.observe("✻ Welcome\n❯ "));
        // The trust screen resets it, however long it stays.
        let trust = "Quicksafetycheck:Isthisaprojectyoucreatedoroneyoutrust?❯No,exit";
        assert!(!s.observe(trust));
        assert!(!s.observe(trust));
        assert!(!s.observe(trust));
        // Once the user answers it, the REPL settles and the kickoff goes.
        assert!(!s.observe("❯ "));
        assert!(s.observe("❯ "));
    }

    #[test]
    fn login_and_first_run_screens_block_the_kickoff() {
        for screen in [
            "Select login method:\n❯ 1. Claude account with subscription",
            "Choose the text style that looks best with your terminal",
            "Do you trust the files in this folder?",
            "OAuth token has expired. Please run /login",
            // claude 2.1.29x trust screen, as the pane renderer captured it.
            "Accessingworkspace:C:\\x\\inv Quicksafetycheck:Isthisaprojectyoucreatedoroneyoutrust?",
            "❯ No, exit\n  Yes, I trust this folder\nEnter to confirm",
            "Selectloginmethod:",
        ] {
            assert!(screen_is_blocking(screen), "{screen}");
        }
        // The ordinary REPL welcome / prompt is not blocking.
        assert!(!screen_is_blocking("✻ Welcome to Claude Code!\n> "));
        assert!(!screen_is_blocking(""));
    }

    #[test]
    fn ready_answer_is_read_from_assistant_turns_only() {
        assert!(line_is_ready_answer(READY_LINE, "scout"));
        assert!(!line_is_ready_answer(READY_LINE, "planner"));
        // A user turn (e.g. the kickoff echo) never counts.
        assert!(!line_is_ready_answer(KICKOFF_ECHO, "scout"));
        assert!(!line_is_ready_answer("not json", "scout"));
    }

    #[test]
    fn transcript_scan_finds_the_answer_in_the_pane_home() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(!transcript_has_ready_answer(home.path(), "scout"));
        let dir = home.path().join(".claude/projects/C--proj");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("abc.jsonl");
        std::fs::write(&file, format!("{KICKOFF_ECHO}\n")).unwrap();
        assert!(!transcript_has_ready_answer(home.path(), "scout"));
        std::fs::write(&file, format!("{KICKOFF_ECHO}\n{READY_LINE}\n")).unwrap();
        assert!(transcript_has_ready_answer(home.path(), "scout"));
    }

    /// Opt-in LIVE run of the real session: 9 real claude panes with the
    /// production personas, permissions and bridge, driven headless.
    /// Spends tokens. Inputs (env): `NEURON_LIVE_PROJECT` (project dir),
    /// `NEURON_LIVE_TASK` (text pasted to the orchestrator),
    /// `NEURON_LIVE_TIMEOUT_MIN` (default 20), `NEURON_LIVE_OUT` (dir for
    /// the transcript copies). Prints the message timeline and every
    /// denied tool call, then stops the session.
    ///
    ///   cargo test --lib swarm_term::session::tests::live -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns 9 real claude REPLs and spends tokens — opt in"]
    async fn live_swarm_run() {
        use std::collections::BTreeMap;
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let project = PathBuf::from(env("NEURON_LIVE_PROJECT"));
        let task = env("NEURON_LIVE_TASK");
        let out = PathBuf::from(env("NEURON_LIVE_OUT"));
        let timeout = Duration::from_secs(
            60 * std::env::var("NEURON_LIVE_TIMEOUT_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20u64),
        );
        let started = Instant::now();
        let stamp = |t: Instant| format!("{:>5.1}s", t.duration_since(started).as_secs_f32());

        // Terminal signals (TASK_DONE + TASK_FAILED) the orchestrator must
        // receive before the run winds down (default: one build task + smoke).
        let expect: usize = std::env::var("NEURON_LIVE_EXPECT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2);

        // `NEURON_LIVE_INJECT` = JSON `{"<agent>": "<extra persona text>"}`.
        if let Ok(raw) = std::env::var("NEURON_LIVE_INJECT") {
            let map: HashMap<String, String> =
                serde_json::from_str(&raw).expect("NEURON_LIVE_INJECT must be a JSON object");
            let mut g = INJECTIONS.lock().unwrap();
            for (agent, extra) in map {
                println!("INJECT [{agent}]: {extra}");
                g.push((agent, extra));
            }
        }

        let (app, _pool, _db) =
            crate::test_support::mock_app_with_pool_and_terminal_registry().await;
        let registry = app.state::<TerminalRegistry>().inner().clone();
        // Bridge-originated pastes (notices, reminders) and lifecycle
        // transitions never touch the filesystem queue — print them too.
        {
            use tauri::Listener;
            let t0 = started;
            app.listen_any(crate::events::SWARM_TERM_ROUTE, move |e| {
                let v: serde_json::Value =
                    serde_json::from_str(e.payload()).unwrap_or_default();
                let outcome = v["outcome"].as_str().unwrap_or("");
                if matches!(outcome, "notice" | "reminder") {
                    let body: String = v["body"].as_str().unwrap_or("").chars().take(220).collect();
                    println!(
                        "[{:>5.1}s] {} → {}: {}",
                        t0.elapsed().as_secs_f32(),
                        outcome.to_uppercase(),
                        v["target"].as_str().unwrap_or("?"),
                        body.replace('\n', " ⏎ ")
                    );
                }
            });
            app.listen_any(crate::events::SWARM_TERM_LIFECYCLE, move |e| {
                let v: serde_json::Value =
                    serde_json::from_str(e.payload()).unwrap_or_default();
                println!(
                    "[{:>5.1}s] LIFECYCLE {} [{}] {} → {}",
                    t0.elapsed().as_secs_f32(),
                    v["source"].as_str().unwrap_or("?"),
                    v["task_id"].as_str().unwrap_or("?"),
                    v["transition"].as_str().unwrap_or("?"),
                    v["state"].as_str().unwrap_or("?"),
                );
            });
        }
        let swarm = Arc::new(TerminalSwarmRegistry::new());
        swarm.start(app.handle().clone(), project.clone()).await.expect("start");
        let (panes, ready, root, homes) = {
            let g = swarm.inner.lock().unwrap();
            let s = g.as_ref().expect("session");
            (
                s.panes_by_agent.clone(),
                Arc::clone(&s.ready_panes),
                s.bridgespace_root.clone().unwrap(),
                s.homes_root.clone().unwrap(),
            )
        };
        println!("[{}] session started: {}", stamp(Instant::now()), root.display());
        // `NEURON_LIVE_RAW_DUMP=<file>`: append the orchestrator pane's raw
        // output stream (escape sequences included) to that file.
        if let (Ok(dump), Some(pane)) = (std::env::var("NEURON_LIVE_RAW_DUMP"), panes.get("orchestrator")) {
            use std::io::Write as _;
            use tauri::Listener;
            app.listen_any(crate::events::pane_data(pane), move |e| {
                let text: String = serde_json::from_str(e.payload()).unwrap_or_default();
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&dump) {
                    let _ = write!(f, "{text}");
                }
            });
        }

        // Readiness. `NEURON_LIVE_ANSWER_TRUST_SECS=<n>` plays the user
        // answering the folder-trust screen late: after n seconds every
        // not-ready pane gets Down + Enter ("Yes, I trust this folder").
        let answer_trust_after = std::env::var("NEURON_LIVE_ANSWER_TRUST_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut trust_answered = false;
        // `NEURON_LIVE_SCREEN_AT=<n>`: print the orchestrator's screen at n s.
        let screen_at = std::env::var("NEURON_LIVE_SCREEN_AT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut screen_shown = false;
        let ready_deadline = Instant::now()
            + Duration::from_secs(150)
            + answer_trust_after.unwrap_or_default();
        loop {
            let n = ready.lock().map(|g| g.len()).unwrap_or(0);
            if n == panes.len() || Instant::now() > ready_deadline {
                println!("[{}] ready panes: {n}/{}", stamp(Instant::now()), panes.len());
                break;
            }
            if !screen_shown && screen_at.is_some_and(|d| started.elapsed() >= d) {
                screen_shown = true;
                if let Some(pane) = panes.get("orchestrator") {
                    let screen = pane_screen_tail(&registry, &_pool, pane).await.unwrap_or_default();
                    let n = screen.chars().count();
                    let tail: String = screen.chars().skip(n.saturating_sub(1500)).collect();
                    println!(
                        "SCREEN [orchestrator] at {} ({n} chars, blocking={}):\n{tail}",
                        stamp(Instant::now()),
                        screen_is_blocking(&screen)
                    );
                }
            }
            if !trust_answered && answer_trust_after.is_some_and(|d| started.elapsed() >= d) {
                trust_answered = true;
                let ready_now = ready.lock().map(|g| g.clone()).unwrap_or_default();
                for pane in panes.values().filter(|p| !ready_now.contains(*p)) {
                    let _ = registry.write_to_pane(pane, b"\x1b[B").await;
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    let _ = registry.write_to_pane(pane, b"\r").await;
                }
                println!("[{}] trust screen answered in {} panes", stamp(Instant::now()), panes.len() - ready_now.len());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // Diagnose panes that never became ready: their screen tail.
        let ready_now = ready.lock().map(|g| g.clone()).unwrap_or_default();
        for (agent, pane) in &panes {
            if ready_now.contains(pane) {
                continue;
            }
            let screen = pane_screen_tail(&registry, &_pool, pane).await.unwrap_or_default();
            let n = screen.chars().count();
            let tail: String = screen.chars().skip(n.saturating_sub(700)).collect();
            println!(
                "NOT READY [{agent}] ({n} chars, blocking={}) screen:\n{tail}",
                screen_is_blocking(&screen)
            );
        }

        // Task → orchestrator, exactly like the chat bar.
        let orch = panes.get("orchestrator").expect("orchestrator pane");
        let paste = format!("\x1b[200~{}\x1b[201~", bridge::sanitize_for_paste(&task));
        bridge::paste_then_submit(&registry, orch, paste.as_bytes()).await.expect("task");
        println!("[{}] task sent to orchestrator", stamp(Instant::now()));

        // Timeline: every file that lands in processed/ or rejected/.
        let mut seen: BTreeMap<PathBuf, ()> = BTreeMap::new();
        let mut task_done = 0usize;
        let mut last_message = Instant::now();
        while started.elapsed() < timeout {
            for kind in ["processed", "rejected"] {
                for target in AGENT_IDS {
                    let dir = root.join(kind).join(target);
                    let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                    let mut files: Vec<PathBuf> = rd
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                        .collect();
                    files.sort();
                    for f in files {
                        if seen.insert(f.clone(), ()).is_some() {
                            continue;
                        }
                        last_message = Instant::now();
                        let raw = std::fs::read_to_string(&f).unwrap_or_default();
                        let v: serde_json::Value =
                            serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                        let body = v["body"].as_str().unwrap_or("").replace('\n', " ⏎ ");
                        let body: String = body.chars().take(170).collect();
                        let from = v["from"].as_str().unwrap_or("?");
                        let reason = if kind == "rejected" {
                            let mut r = f.clone();
                            r.set_extension("reason");
                            format!(" REJECTED: {}", std::fs::read_to_string(r).unwrap_or_default())
                        } else {
                            String::new()
                        };
                        println!(
                            "[{}] {from} → {target} [{}]: {body}{reason}",
                            stamp(Instant::now()),
                            v["task_id"].as_str().map(str::to_string)
                                .unwrap_or_else(|| v["task_id"].to_string()),
                        );
                        let head = body.trim_start();
                        if kind == "processed"
                            && *target == "orchestrator"
                            && (head.starts_with("TASK_DONE") || head.starts_with("TASK_FAILED"))
                        {
                            task_done += 1;
                        }
                    }
                }
            }
            // Every expected terminal signal arrived and the swarm has been
            // quiet for a minute (a retry may add signals): the
            // orchestrator has written its final summary.
            if task_done >= expect && last_message.elapsed() > Duration::from_secs(60) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        println!(
            "[{}] TASK_DONE/TASK_FAILED received by orchestrator: {task_done}/{expect}",
            stamp(Instant::now())
        );

        // Denied tool calls + transcript copies, before stop() deletes the homes.
        std::fs::create_dir_all(&out).ok();
        for agent in AGENT_IDS {
            let dir = homes.join(agent).join(".claude").join("projects");
            let Ok(projects) = std::fs::read_dir(&dir) else { continue };
            for p in projects.flatten() {
                for f in std::fs::read_dir(p.path()).into_iter().flatten().flatten() {
                    // Only the session transcript; a sibling directory /
                    // other file must not overwrite the copy with "".
                    if f.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(f.path()) else { continue };
                    std::fs::write(out.join(format!("{agent}.jsonl")), &text).ok();
                    // The orchestrator's last words are the run's summary.
                    if *agent == "orchestrator" {
                        let last = text
                            .lines()
                            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                            .filter(|v| v["type"] == "assistant")
                            .flat_map(|v| {
                                v.pointer("/message/content")
                                    .and_then(|c| c.as_array().cloned())
                                    .unwrap_or_default()
                            })
                            .filter_map(|p| p["text"].as_str().map(str::to_string))
                            .next_back();
                        if let Some(last) = last {
                            println!("ORCHESTRATOR FINAL:\n{last}");
                        }
                    }
                    for line in text.lines() {
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                        let Some(parts) = v.pointer("/message/content").and_then(|c| c.as_array())
                        else { continue };
                        for part in parts {
                            if part["type"] == "tool_result" && part["is_error"] == true {
                                let c = part["content"].to_string();
                                if c.to_lowercase().contains("permission") || c.contains("denied") {
                                    let c: String = c.chars().take(200).collect();
                                    println!("DENIED [{agent}] {c}");
                                }
                            }
                        }
                    }
                }
            }
        }

        swarm.stop(app.handle().clone()).await.expect("stop");
        println!("[{}] session stopped", stamp(Instant::now()));
    }
}

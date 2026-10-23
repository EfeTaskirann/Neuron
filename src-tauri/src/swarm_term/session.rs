//! `TerminalSwarmSession` lifecycle.
//!
//! `start()` spawns one PTY pane per agent of the session's team (all nine
//! of `hierarchy::AGENT_IDS`, or the Lite team — see [`team`]) through
//! `TerminalRegistry::spawn_pane`, each running interactive
//! `claude` with the user-selected project as `cwd` — in a folder the
//! user has trusted ([`trust`]) and on the user's own Claude
//! configuration ([`pane_config`]). Before the spawn loop it prepares
//! `<project>/.bridgespace/<session>/{outbox,processed,rejected}/<agent>/`
//! and passes per-pane environment variables (`NEURON_BRIDGE`,
//! `NEURON_AGENT_ID`, `NEURON_OUTBOX`) so each claude REPL knows where to
//! drop outbound messages. Each pane's persona + bridge protocol is its
//! appended system prompt (`--append-system-prompt-file`); a short
//! kickoff turn is then pasted and the pane counts as ready once claude
//! has answered it (asked for `@<id> READY`) in its transcript, named by
//! the pane's `--session-id`. The [`bridge::watcher::watcher_loop`] picks up the
//! JSON envelopes those drops create and delivers their body into the
//! appropriate target pane via bracketed paste.
//!
//! `stop()` kills every pane, uninstalls the bridge watcher, archives
//! `rejected/`, and deletes the `.bridgespace/<session>/` tree, the
//! panes' transcripts and the session's Neuron files.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
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
    self, BridgeControl, BridgeHandle, PauseReason, SharedPanes, BRIDGE_DIRNAME,
};
use crate::swarm_term::checkpoint;
use crate::swarm_term::experience;
use crate::swarm_term::heartbeat;
use crate::swarm_term::hierarchy::{builders_judged_by, AGENT_IDS};
use crate::swarm_term::lifecycle::{LifecycleState, LifecycleStore, TESTER};
use crate::swarm_term::pane_config::{self, PaneManifest};
use crate::swarm_term::pane_state::{PaneReadiness, PaneStates, TerminalSwarmPaneState};
use crate::swarm_term::permissions::{self, SwarmTermPermissionMode};
use crate::swarm_term::project_guard;
use crate::swarm_term::persona::{
    build_kickoff_message, build_persona_prompt, build_restart_kickoff, is_ready_answer,
};
use crate::swarm_term::preflight;
use crate::swarm_term::project_facts;
use crate::swarm_term::repin::{self, Compaction};
use crate::swarm_term::team;
use crate::swarm_term::tokens::{self, Crossing, Overrun, TokenLedger};
use crate::swarm_term::trust;
use crate::swarm_term::waits;

const READY_DELAY_MS: u64 = 1500;

/// How long to wait for a pane's `@<id> READY` answer after its
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

/// Claude's folder-trust screen is answered for the user (who trusted the
/// folder in Neuron already) at most this often and this many times per
/// pane — a screen that does not go away is left to the user.
const TRUST_ANSWER_EVERY: Duration = Duration::from_secs(3);
const TRUST_ANSWER_ATTEMPTS: u32 = 3;

/// Removing a stopped session's transcripts right after killing the panes
/// can fail with "file in use" (os error 32) while the claude processes
/// are still exiting; retry this many times, [`CLEANUP_RETRY_DELAY`]
/// apart.
const CLEANUP_ATTEMPTS: u32 = 10;
const CLEANUP_RETRY_DELAY: Duration = Duration::from_millis(500);

/// Delay after the readiness wait before the auto-prompt
/// (`NEURON_TERM_AUTO_PROMPT`) is pasted into the orchestrator pane.
/// Only consulted when the env var is set, so production paths pay
/// zero cost.
const AUTO_PROMPT_DELAY_MS: u64 = 2_000;

/// Gap between consecutive `claude.exe` PTY spawns. The panes share the
/// user's `~/.claude.json`, which claude writes (startup counter, tips
/// etc.) within the first ~300 ms of process boot. Early claude builds
/// wrote it non-atomically, and 9 simultaneous boots left it as
/// concatenated `...}\n}\n}` — invalid JSON that stopped claude from
/// starting at all. claude 2.1.296 survives simultaneous boots (live
/// stress test, `tests::live_simultaneous_boots_keep_the_shared_config_valid`);
/// the gap stays as a margin and is imperceptible against the boot time.
const SPAWN_STAGGER_MS: u64 = 500;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSwarmSessionHandle {
    pub session_id: String,
    pub project_dir: String,
    pub panes: Vec<TerminalSwarmPane>,
    /// What the panes may do — the mode they were spawned with.
    pub permission_mode: SwarmTermPermissionMode,
    /// The `claude` CLI version the panes run (pre-flight check), when
    /// it could be read.
    pub claude_version: Option<String>,
    /// A pre-flight finding that did not block the start (an untested
    /// newer claude, a check that could not run) — for the UI to show.
    pub preflight_note: Option<String>,
    /// Every pane's readiness (see `pane_state`); updates arrive as
    /// `swarm-term:pane-state` events.
    pub pane_states: Vec<TerminalSwarmPaneState>,
    /// The account's usage limit is hit: deliveries and the stall clocks
    /// are held until an agent gets through again (or the user resumes).
    pub usage_limited: bool,
    /// When claude said the limit resets (unix seconds), if it did.
    pub limit_resets_at: Option<i64>,
    /// Why the session has no checkpoint (a folder without git), or what
    /// its snapshots leave out (see `checkpoint`).
    pub changes_note: Option<String>,
    /// The session runs the Lite team (see `team`).
    pub lite_team: bool,
    /// The user paused the swarm: no message is delivered.
    pub paused: bool,
    /// Agent-to-agent messages delivered so far, and the session's budget
    /// (`0`: no limit). `budget_reached`: the swarm holds until the user
    /// gives it more.
    pub messages_delivered: u32,
    pub message_budget: u32,
    pub budget_reached: bool,
    /// Tokens every agent's answers cost so far (input + cache + output)
    /// and the per-task budget the session runs with (`0`: none). See
    /// `tokens`.
    pub tokens_used: u64,
    pub task_token_budget: u64,
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

/// What a pane's claude command is made of, kept so a restart builds
/// exactly the command the session started with.
#[derive(Clone)]
pub(crate) struct PaneSpawnSpec {
    /// The quoted claude program and its prefix args.
    pub program: String,
    /// The project's verification / install commands (permission
    /// allowlist).
    pub facts: project_facts::ProjectFacts,
    /// Agent id → its `--model` alias (see `team`).
    pub models: BTreeMap<String, String>,
}

pub(crate) struct ActiveSession {
    pub session_id: String,
    pub project_dir: PathBuf,
    /// Agent id → pane id, shared with the bridge watcher (a restarted
    /// agent runs in a new pane).
    pub panes: SharedPanes,
    /// Agent id → the `--session-id` its claude runs with; a restart
    /// resumes the same conversation.
    pub claude_sessions: BTreeMap<String, String>,
    pub spawn: PaneSpawnSpec,
    pub lifecycle: Arc<LifecycleStore>,
    /// Holds the bridge's deliveries (usage limit, the user).
    pub control: Arc<BridgeControl>,
    /// The account's usage limit, read from the transcripts by
    /// [`monitor_session`].
    pub limit: Arc<Mutex<UsageLimit>>,
    /// Agents being restarted right now — one restart at a time each.
    pub restarting: Arc<Mutex<HashSet<String>>>,
    /// Permission mode every pane of this session was spawned with.
    pub permission_mode: SwarmTermPermissionMode,
    /// Bridge watcher handle. Holding it keeps the watcher task alive;
    /// `bridge::uninstall` in `stop()` cancels the task. Optional only
    /// to keep the construction order flexible — once the session is
    /// inserted into the registry the field is always `Some`.
    pub bridge: Option<BridgeHandle>,
    /// Per-session `.bridgespace/<session>/` root. Cleaned up in
    /// `stop()` along with `session_dir`.
    pub bridgespace_root: Option<PathBuf>,
    /// The session's Neuron files (`app_data/swarm-term/sessions/<id>`:
    /// persona prompts, pane manifest, heartbeat). Removed in `stop()`
    /// together with the transcripts the manifest names. `None` for
    /// synthetic / test sessions that bypass the spawn loop.
    pub session_dir: Option<PathBuf>,
    /// Where the panes' transcripts are looked up (and deleted on stop).
    pub projects_dirs: Vec<PathBuf>,
    /// See [`TerminalSwarmSessionHandle::claude_version`] /
    /// [`TerminalSwarmSessionHandle::preflight_note`].
    pub claude_version: Option<String>,
    pub preflight_note: Option<String>,
    /// See [`TerminalSwarmSessionHandle::changes_note`].
    pub changes_note: Option<String>,
    /// See [`TerminalSwarmSessionHandle::lite_team`].
    pub lite_team: bool,
    /// The message budget (the user may raise it) and the deliveries so
    /// far, counted by [`monitor_session`]; `budget_step` is the budget
    /// the settings gave, by which the user raises it.
    pub budget: Arc<AtomicU32>,
    pub delivered: Arc<AtomicU32>,
    pub budget_step: u32,
    /// Tokens spent, per task and in all (see `tokens`), counted by
    /// [`monitor_session`].
    pub tokens: Arc<Mutex<TokenLedger>>,
    /// Every pane's readiness, published by the kickoff task.
    pub pane_states: Arc<PaneStates>,
    /// Pane ids that have answered their kickoff and are safe targets for
    /// inter-agent routes. Filled by the readiness waits; read by
    /// `bridge::watcher::process_one` to short-circuit routes destined for
    /// panes that are still warming up (the claude REPL may not have
    /// enabled bracketed-paste mode yet, in which case the routed
    /// body is dumped into the prompt as raw text and submitted with
    /// the first `\r`, corrupting the receiver's first dispatch). A pane
    /// whose process ended leaves it ([`monitor_session`]), and a restart
    /// swaps its agent's entry.
    pub ready_panes: Arc<Mutex<HashSet<String>>>,
    /// Cleared by `stop()`. The session's background tasks (kickoff /
    /// readiness wait, heartbeat) poll it and exit, so a stopped
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
        // ... and the user must have trusted it in Neuron (the UI asks
        // on this error, records the answer and starts again).
        let pool = app.state::<DbPool>().inner().clone();
        if !trust::is_trusted(&pool, &project_dir).await? {
            return Err(AppError::ConsentRequired(project_guard::display_path(&project_dir)));
        }
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
        // One clear error instead of nine broken panes: a claude too old
        // for the pane flags, or no login (see `preflight`).
        let preflight = preflight::run(&spawn).await?;
        if let Some(note) = &preflight.note {
            tracing::warn!("swarm-term pre-flight: {note}");
        }
        // What the project is and how it is verified, read from its
        // manifests (see `project_facts`) — and a probe of its cheap
        // checks before nine panes open: a missing toolchain is one clear
        // error here, not nine failed tasks later.
        let mut facts = project_facts::detect(&project_dir);
        let env = preflight::probe_environment(&project_dir, &facts).await?;
        if !env.broken.is_empty() {
            tracing::warn!(broken = ?env.broken, "swarm-term pre-flight: checks failing before the start");
        }
        facts.broken = env.broken;
        // The user's cost controls: the team, its models, the message budget.
        let team_settings = team::load(&pool).await?;
        let agents = team_settings.agents();
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
            "swarm-term: spawning the team's claude REPLs"
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

        // ---- Bridge filesystem layout ------------------------------- //
        // The per-session bridge root lives under the user's project
        // root so the agents' Write tool can target it with a path
        // that's natural in the cwd. A `.gitignore` at the parent
        // keeps per-session message files out of `git status`.
        let session_id = format!("swarm-term-{}", Ulid::new());
        let bridge_parent = project_dir.join(BRIDGE_DIRNAME);
        // What earlier sessions learned here goes into every persona (see
        // `experience`); what the probe found broken now is remembered.
        facts.experience = experience::lines_for_persona(&experience::load(&bridge_parent));
        for broken in &facts.broken {
            experience::record(&bridge_parent, experience::Kind::Broken, &format!("başlangıçta kırık: {broken}"));
        }
        std::fs::create_dir_all(&bridge_parent).map_err(|e| {
            AppError::Internal(format!(
                "mkdir {}: {e}",
                bridge_parent.display()
            ))
        })?;
        let _ = bridge::ensure_gitignore(&bridge_parent);
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|e| AppError::Internal(format!("app_data_dir: {e}")))?;
        let swarm_term_dir = pane_config::swarm_term_data_dir(&app_data);
        // claude's own answer first (`auth status`), the usual spots after.
        let mut projects_dirs = pane_config::claude_projects_dirs();
        if let Some(dir) = preflight.projects_dir.clone() {
            projects_dirs.retain(|d| d != &dir);
            projects_dirs.insert(0, dir);
        }
        // No session is running (the reservation above guarantees it),
        // so every leftover is an orphan of a session that never reached
        // `stop()`: its bridge tree, its transcripts and Neuron files, and
        // the pane-HOME copies older builds made.
        let orphan_sessions = bridge::sweep_orphan_sessions(&bridge_parent);
        let swept = pane_config::sweep_orphans(&swarm_term_dir, &projects_dirs);
        if orphan_sessions + swept.sessions + swept.legacy_homes > 0 {
            tracing::info!(
                orphan_sessions,
                orphan_session_files = swept.sessions,
                legacy_pane_homes = swept.legacy_homes,
                "swarm-term: swept leftovers of sessions that never stopped"
            );
        }
        let bridgespace_root = bridge_parent.join(&session_id);
        bridge::prepare_layout(&bridgespace_root)?;
        heartbeat::beat(&bridgespace_root);

        // The way back: the project as it is now, before any agent runs
        // (see `checkpoint`). A folder without git runs without one, and
        // the UI says so.
        let changes_note = {
            let (app_data, project, sid) = (app_data.clone(), project_dir.clone(), session_id.clone());
            match tokio::task::spawn_blocking(move || checkpoint::take(&app_data, &project, &sid)).await {
                Ok(Ok(note)) => note,
                Ok(Err(why)) => {
                    tracing::warn!(why = %why, "swarm-term: the session has no checkpoint");
                    Some(why)
                }
                Err(e) => Some(format!("checkpoint: {e}")),
            }
        };

        // The session's own files and every pane's claude session id —
        // on disk before any pane runs, so even after a crash the next
        // start knows which transcripts to delete.
        let session_dir = pane_config::session_dir(&app_data, &session_id);
        let claude_sessions: BTreeMap<String, String> = agents
            .iter()
            .map(|agent| (agent.to_string(), pane_config::new_claude_session_id()))
            .collect();
        let prepared = std::fs::create_dir_all(&session_dir)
            .map_err(|e| AppError::Internal(format!("mkdir {}: {e}", session_dir.display())))
            .and_then(|()| {
                PaneManifest { claude_sessions: claude_sessions.clone() }.write(&session_dir)
            });
        if let Err(e) = prepared {
            abandon_start(&registry, &pool, &[], &projects_dirs, &session_dir, &bridgespace_root)
                .await;
            return Err(e);
        }
        heartbeat::beat(&session_dir);

        // The facts (detected and probed before the preparation above):
        // the footer tells every agent, and the shell roles may run
        // exactly these commands.
        tracing::info!(
            stacks = ?facts.stacks,
            commands = ?facts.commands,
            installs = ?facts.install_commands,
            "swarm-term: project facts"
        );
        // Every pane's claude command is made of these (a restart, too).
        let spec = PaneSpawnSpec {
            program: cmd.clone(),
            facts: facts.clone(),
            models: team_settings.effective_models(),
        };
        // Personas are loaded BEFORE spawning: each pane gets its
        // persona + bridge protocol as an appended SYSTEM prompt
        // (`--append-system-prompt-file`), so it must exist on disk
        // when claude starts.
        let workspace_agents_dir = Some(swarm_term_dir.join("agents")).filter(|p| p.is_dir());
        let profiles =
            match ProfileRegistry::load_term(workspace_agents_dir.as_deref()) {
                Ok(r) => r,
                Err(e) => {
                    abandon_start(
                        &registry,
                        &pool,
                        &[],
                        &projects_dirs,
                        &session_dir,
                        &bridgespace_root,
                    )
                    .await;
                    return Err(AppError::Internal(format!(
                        "load swarm-term personas: {e}"
                    )));
                }
            };

        let mut panes_by_agent: HashMap<String, String> = HashMap::new();
        let mut spawned: Vec<String> = Vec::new();
        for (idx, &agent_id) in agents.iter().enumerate() {
            if idx > 0 {
                tokio::time::sleep(Duration::from_millis(SPAWN_STAGGER_MS)).await;
            }
            // Persona + bridge protocol → system-prompt file in the
            // session's Neuron dir (deleted with it on stop).
            let persona_file = pane_config::persona_file(&session_dir, agent_id);
            let persona_written = profiles
                .get(agent_id)
                .ok_or_else(|| format!("no persona profile for `{agent_id}`"))
                .and_then(|profile| {
                    let prompt = build_persona_prompt(
                        agent_id,
                        &profile.body,
                        &bridgespace_root,
                        &facts,
                        &agents,
                    );
                    #[cfg(test)]
                    let prompt = live::with_injection(agent_id, prompt);
                    std::fs::write(&persona_file, prompt)
                        .map_err(|e| format!("write {}: {e}", persona_file.display()))
                });
            if let Err(e) = persona_written {
                abandon_start(
                    &registry,
                    &pool,
                    &spawned,
                    &projects_dirs,
                    &session_dir,
                    &bridgespace_root,
                )
                .await;
                return Err(AppError::Internal(format!(
                    "swarm-term persona for `{agent_id}`: {e}"
                )));
            }
            let agent_cmd = agent_command(
                &spec,
                permission_mode,
                agent_id,
                &session_id,
                pane_config::claude_args(&claude_sessions[agent_id], &persona_file),
            );
            let input = agent_pane_input(&project_dir, &bridgespace_root, agent_id, agent_cmd);
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
                    abandon_start(
                        &registry,
                        &pool,
                        &spawned,
                        &projects_dirs,
                        &session_dir,
                        &bridgespace_root,
                    )
                    .await;
                    return Err(AppError::Internal(format!(
                        "spawn agent `{agent_id}`: {e}"
                    )));
                }
            }
        }

        // Per-session "ready" tracker — a pane joins once it has
        // answered the kickoff. Routes to a not-yet-ready pane
        // short-circuit with `target_not_ready` in
        // `bridge::watcher::process_one` so the pre-bracketed-paste-mode
        // window doesn't corrupt the first inter-agent dispatch.
        let ready_panes: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));
        // Per-session lifecycle store — drives the autonomy contract
        // (Builder→Reviewer / Reviewer→Orchestrator fanouts). Owned by
        // the session and read by the bridge watcher via the Arc clone
        // handed to `bridge::install`.
        let lifecycle = Arc::new(LifecycleStore::new());

        // Install the bridge watcher BEFORE inserting the session
        // — once it's live, any envelope dropped into an outbox fires
        // a delivery. The kickoff (below) is what starts the chain, so
        // the watcher needs to be ready first.
        let shared_panes: SharedPanes = Arc::new(RwLock::new(panes_by_agent.clone()));
        let control = Arc::new(BridgeControl::default());
        let bridge_handle = bridge::install(
            app.clone(),
            bridgespace_root.clone(),
            Arc::clone(&shared_panes),
            Arc::clone(&ready_panes),
            Arc::clone(&lifecycle),
            Arc::clone(&control),
        );

        let alive = Arc::new(AtomicBool::new(true));
        let pane_states = Arc::new(PaneStates::new(agents.iter().copied()));
        let budget = Arc::new(AtomicU32::new(team_settings.message_budget));
        let delivered = Arc::new(AtomicU32::new(0));
        let limit = Arc::new(Mutex::new(UsageLimit::default()));
        let restarting = Arc::new(Mutex::new(HashSet::new()));
        let tokens = Arc::new(Mutex::new(TokenLedger::new(team_settings.task_token_budget)));
        let session = ActiveSession {
            session_id: session_id.clone(),
            project_dir: project_dir.clone(),
            panes: Arc::clone(&shared_panes),
            claude_sessions: claude_sessions.clone(),
            spawn: spec,
            lifecycle: Arc::clone(&lifecycle),
            control: Arc::clone(&control),
            limit: Arc::clone(&limit),
            restarting: Arc::clone(&restarting),
            permission_mode,
            bridge: Some(bridge_handle),
            bridgespace_root: Some(bridgespace_root.clone()),
            session_dir: Some(session_dir.clone()),
            projects_dirs: projects_dirs.clone(),
            claude_version: preflight.version.map(|v| v.to_string()),
            preflight_note: preflight.note.clone(),
            changes_note,
            lite_team: team_settings.lite,
            budget: Arc::clone(&budget),
            delivered: Arc::clone(&delivered),
            budget_step: team_settings.message_budget,
            tokens: Arc::clone(&tokens),
            pane_states: Arc::clone(&pane_states),
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
        let _ = tauri::Emitter::emit(
            &app,
            crate::events::SWARM_TERM_SESSION,
            serde_json::json!({ "sessionId": session_id, "state": "started" }),
        );

        // Fire-and-forget kickoff: wait for the claude REPLs to settle,
        // paste the short kickoff turn into every pane, then mark each
        // pane ready only once claude has actually ANSWERED it with
        // `@<id> READY` (read from the pane's own transcript — the
        // terminal echo of the kickoff would be a false positive). A
        // pane stuck on a first-run screen therefore never becomes a
        // delivery target; messages to it are returned to their sender.
        let publish = pane_publisher(
            app.clone(),
            Arc::clone(&shared_panes),
            Arc::clone(&pane_states),
            session_id.clone(),
            Arc::clone(&alive),
        );
        let wait = ReadyWait {
            registry: registry.clone(),
            pool: pool.clone(),
            panes: Arc::clone(&shared_panes),
            claude_sessions: claude_sessions.clone(),
            projects_dirs: projects_dirs.clone(),
            ready: Arc::clone(&ready_panes),
            alive: Arc::clone(&alive),
            restarting: Arc::clone(&restarting),
            publish: Arc::clone(&publish),
        };
        let awaited: HashMap<String, Awaited> = panes_by_agent
            .iter()
            .map(|(agent, pane)| (agent.clone(), Awaited { pane: pane.clone(), from_byte: 0 }))
            .collect();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(READY_DELAY_MS)).await;
            if !wait.alive.load(Ordering::SeqCst) {
                return;
            }
            // No kickoff is pasted blind: the wait loop sends each one once
            // that pane's screen has settled (see `SETTLED_CHECKS`).
            let late = wait_for_ready_answers(&wait, &awaited, build_kickoff_message).await;
            if !wait.alive.load(Ordering::SeqCst) {
                return;
            }

            if let Ok(auto_prompt) = std::env::var("NEURON_TERM_AUTO_PROMPT") {
                let trimmed = auto_prompt.trim();
                if !trimmed.is_empty() {
                    tokio::time::sleep(Duration::from_millis(AUTO_PROMPT_DELAY_MS)).await;
                    let orch_pane = wait
                        .panes
                        .read()
                        .unwrap_or_else(|p| p.into_inner())
                        .get("orchestrator")
                        .cloned();
                    if let Some(orch_pane) = orch_pane {
                        let payload = format!(
                            "\x1b[200~{}\x1b[201~",
                            bridge::sanitize_for_paste(trimmed)
                        );
                        if let Err(e) = bridge::paste_then_submit(
                            &wait.registry,
                            &orch_pane,
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
            watch_late_answers(&wait, &awaited, late).await;
        });

        // Exits and the usage limit, for the whole session.
        tauri::async_runtime::spawn(monitor_session(SessionMonitor {
            app: app.clone(),
            registry: registry.clone(),
            panes: Arc::clone(&shared_panes),
            claude_sessions: claude_sessions.clone(),
            projects_dirs: projects_dirs.clone(),
            ready: Arc::clone(&ready_panes),
            alive: Arc::clone(&alive),
            publish,
            session_id: session_id.clone(),
            control: Arc::clone(&control),
            lifecycle: Arc::clone(&lifecycle),
            limit: Arc::clone(&limit),
            restarting,
            bridgespace_root: bridgespace_root.clone(),
            budget,
            delivered,
            tokens,
        }));

        // Session upkeep, ends with the session: heartbeat both on-disk
        // trees so another Neuron instance's orphan sweep leaves them
        // alone (see `heartbeat`).
        let alive_for_upkeep = Arc::clone(&alive);
        let bridge_root_for_upkeep = bridgespace_root.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(heartbeat::HEARTBEAT_EVERY).await;
                if !alive_for_upkeep.load(Ordering::SeqCst) {
                    return;
                }
                heartbeat::beat(&session_dir);
                heartbeat::beat(&bridge_root_for_upkeep);
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
        let (session_id, pane_ids, bridge, session_dir, projects_dirs, bridgespace_root) = {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            match guard.take() {
                Some(s) => {
                    s.alive.store(false, Ordering::SeqCst);
                    let pane_ids: Vec<String> =
                        s.panes.read().unwrap_or_else(|p| p.into_inner()).values().cloned().collect();
                    (
                        s.session_id,
                        pane_ids,
                        s.bridge,
                        s.session_dir,
                        s.projects_dirs,
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
        if let Some(dir) = session_dir {
            remove_session_with_retry(&projects_dirs, &dir).await;
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
        let _ = tauri::Emitter::emit(
            &app,
            crate::events::SWARM_TERM_SESSION,
            serde_json::json!({ "sessionId": session_id, "state": "stopped" }),
        );
        Ok(())
    }

    /// Restart one agent: kill what is left of its pane and run its claude
    /// again in a new pane on the same conversation (`--resume`; a fresh
    /// `--session-id` when claude never wrote one) — after its process
    /// ended, or when it never got ready. The new pane gets the restart
    /// kickoff and is awaited like at the start; the agent's tasks move
    /// with it.
    pub async fn restart_agent<R: Runtime>(
        &self,
        app: AppHandle<R>,
        agent_id: &str,
    ) -> Result<TerminalSwarmSessionHandle, AppError> {
        if !AGENT_IDS.contains(&agent_id) {
            return Err(AppError::InvalidInput(format!("unknown agent `{agent_id}`")));
        }
        // (An agent the session's team leaves out has no pane: NotFound.)
        let (ctx, _restarting) = {
            let guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            let s = guard.as_ref().ok_or_else(no_session)?;
            let (Some(bridgespace_root), Some(session_dir)) =
                (s.bridgespace_root.clone(), s.session_dir.clone())
            else {
                return Err(AppError::Conflict(
                    "this terminal-swarm session has no pane files to restart from".into(),
                ));
            };
            let old_pane = s
                .panes
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .get(agent_id)
                .cloned()
                .ok_or_else(|| AppError::NotFound(format!("no pane for `{agent_id}`")))?;
            let claude_session = s
                .claude_sessions
                .get(agent_id)
                .cloned()
                .ok_or_else(|| AppError::NotFound(format!("no claude session for `{agent_id}`")))?;
            let restarting = RestartGuard::claim(&s.restarting, agent_id).ok_or_else(|| {
                AppError::Conflict(format!("`{agent_id}` is already restarting"))
            })?;
            let ctx = RestartCtx {
                session_id: s.session_id.clone(),
                project_dir: s.project_dir.clone(),
                old_pane,
                claude_session,
                spawn: s.spawn.clone(),
                permission_mode: s.permission_mode,
                bridgespace_root,
                session_dir,
                panes: Arc::clone(&s.panes),
                claude_sessions: s.claude_sessions.clone(),
                projects_dirs: s.projects_dirs.clone(),
                lifecycle: Arc::clone(&s.lifecycle),
                pane_states: Arc::clone(&s.pane_states),
                ready: Arc::clone(&s.ready_panes),
                restarting: Arc::clone(&s.restarting),
                alive: Arc::clone(&s.alive),
            };
            (ctx, restarting)
        };
        let registry = app.state::<TerminalRegistry>().inner().clone();
        let pool = app.state::<DbPool>().inner().clone();

        if let Ok(mut g) = ctx.ready.lock() {
            g.remove(&ctx.old_pane);
        }
        let _ = registry.kill_pane(&ctx.old_pane, &pool).await;
        // How far the transcript the new pane resumes has come, read AFTER
        // the old process is gone: anything it wrote on the way out then
        // counts as the old turn, not as the restarted pane's `READY`.
        let transcript_len = pane_config::find_transcript(&ctx.projects_dirs, &ctx.claude_session)
            .and_then(|file| std::fs::metadata(file).ok())
            .map(|m| m.len());

        let persona_file = pane_config::persona_file(&ctx.session_dir, agent_id);
        let session_args = match transcript_len {
            Some(_) => pane_config::claude_resume_args(&ctx.claude_session, &persona_file),
            None => pane_config::claude_args(&ctx.claude_session, &persona_file),
        };
        let cmd = agent_command(&ctx.spawn, ctx.permission_mode, agent_id, &ctx.session_id, session_args);
        let input = agent_pane_input(&ctx.project_dir, &ctx.bridgespace_root, agent_id, cmd);
        let publish = pane_publisher(
            app.clone(),
            Arc::clone(&ctx.panes),
            Arc::clone(&ctx.pane_states),
            ctx.session_id.clone(),
            Arc::clone(&ctx.alive),
        );
        let new_pane = match registry.clone().spawn_pane(input, app.clone(), pool.clone()).await {
            Ok(pane) => pane.id,
            // The old pane is still the agent's in the map; the session
            // monitor reports its exit once (with the generic reason),
            // and the command's Err carries the restart failure to the
            // UI's error banner — so no second, reason-losing toast here.
            Err(e) => return Err(AppError::Internal(format!("restart agent `{agent_id}`: {e}"))),
        };
        let swapped = {
            let mut panes = ctx.panes.write().unwrap_or_else(|p| p.into_inner());
            // Under the map's lock: `stop()` clears `alive` before it reads
            // the map, so it either kills the new pane or we see it stopping.
            // The watcher never sees the new pane without the agent's tasks.
            let live = ctx.alive.load(Ordering::SeqCst);
            if live {
                panes.insert(agent_id.to_string(), new_pane.clone());
                ctx.lifecycle.rename_pane(&ctx.old_pane, &new_pane);
            }
            live
        };
        if !swapped {
            let _ = registry.kill_pane(&new_pane, &pool).await;
            return Err(AppError::Conflict("the terminal-swarm session was stopped".into()));
        }
        publish(agent_id, &new_pane, PaneReadiness::Starting, None);
        tracing::info!(
            agent_id,
            old_pane = %ctx.old_pane,
            new_pane = %new_pane,
            resumed = transcript_len.is_some(),
            "swarm-term: agent restarted"
        );

        let wait = ReadyWait {
            registry,
            pool,
            panes: Arc::clone(&ctx.panes),
            claude_sessions: ctx.claude_sessions.clone(),
            projects_dirs: ctx.projects_dirs.clone(),
            ready: Arc::clone(&ctx.ready),
            alive: Arc::clone(&ctx.alive),
            restarting: Arc::clone(&ctx.restarting),
            publish,
        };
        let awaited = HashMap::from([(
            agent_id.to_string(),
            Awaited { pane: new_pane, from_byte: transcript_len.unwrap_or(0) },
        )]);
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(READY_DELAY_MS)).await;
            let late = wait_for_ready_answers(&wait, &awaited, build_restart_kickoff).await;
            watch_late_answers(&wait, &awaited, late).await;
        });
        self.current().ok_or_else(no_session)
    }

    /// The user's "go on" while the usage limit holds the swarm: deliveries
    /// and the stall clocks run again. If the account is still limited,
    /// the next limit error holds them again.
    pub fn resume_deliveries<R: Runtime>(
        &self,
        app: &AppHandle<R>,
    ) -> Result<TerminalSwarmSessionHandle, AppError> {
        let guard = self.inner.lock().map_err(|_| {
            AppError::Internal("swarm-term registry poisoned".into())
        })?;
        let s = guard.as_ref().ok_or_else(no_session)?;
        {
            let mut limit = s.limit.lock().unwrap_or_else(|p| p.into_inner());
            if limit.limited {
                limit.clear();
                hold_deliveries(&s.control, &s.lifecycle, PauseReason::UsageLimit, false);
                tracing::info!("swarm-term: the user resumed the swarm during the usage limit");
                emit_limit(app, &s.session_id, &limit);
            }
        }
        Ok(handle_from(s))
    }

    /// The user's Pause / Resume: while paused no message is delivered —
    /// they wait in the outboxes, in order — and the stall clocks stand
    /// still. An agent in the middle of a turn finishes it.
    pub fn set_paused<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        paused: bool,
    ) -> Result<TerminalSwarmSessionHandle, AppError> {
        let guard = self.inner.lock().map_err(|_| {
            AppError::Internal("swarm-term registry poisoned".into())
        })?;
        let s = guard.as_ref().ok_or_else(no_session)?;
        {
            // Every hold changes under the limit's lock (see `monitor_session`).
            let _holds = s.limit.lock().unwrap_or_else(|p| p.into_inner());
            if s.control.paused_by(PauseReason::User) != paused {
                hold_deliveries(&s.control, &s.lifecycle, PauseReason::User, paused);
                tracing::info!(paused, "swarm-term: the user paused / resumed the swarm");
                emit_session_state(app, &s.session_id, if paused { "paused" } else { "running" });
            }
        }
        Ok(handle_from(s))
    }

    /// The user's "more messages": the budget grows by its configured size
    /// from what was delivered, and the swarm runs again.
    pub fn extend_budget<R: Runtime>(
        &self,
        app: &AppHandle<R>,
    ) -> Result<TerminalSwarmSessionHandle, AppError> {
        let guard = self.inner.lock().map_err(|_| {
            AppError::Internal("swarm-term registry poisoned".into())
        })?;
        let s = guard.as_ref().ok_or_else(no_session)?;
        {
            let _holds = s.limit.lock().unwrap_or_else(|p| p.into_inner());
            if s.budget_step > 0 {
                let delivered = s.delivered.load(Ordering::SeqCst);
                s.budget.store(delivered.saturating_add(s.budget_step), Ordering::SeqCst);
            }
            if s.control.paused_by(PauseReason::Budget) {
                hold_deliveries(&s.control, &s.lifecycle, PauseReason::Budget, false);
                tracing::info!(
                    budget = s.budget.load(Ordering::SeqCst),
                    "swarm-term: the user raised the message budget"
                );
                emit_session_state(app, &s.session_id, "running");
            }
        }
        Ok(handle_from(s))
    }
}

fn no_session() -> AppError {
    AppError::NotFound("no terminal-swarm session is running".into())
}

/// What a restart takes from the running session.
struct RestartCtx {
    session_id: String,
    project_dir: PathBuf,
    old_pane: String,
    claude_session: String,
    spawn: PaneSpawnSpec,
    permission_mode: SwarmTermPermissionMode,
    bridgespace_root: PathBuf,
    session_dir: PathBuf,
    panes: SharedPanes,
    claude_sessions: BTreeMap<String, String>,
    projects_dirs: Vec<PathBuf>,
    lifecycle: Arc<LifecycleStore>,
    pane_states: Arc<PaneStates>,
    ready: Arc<Mutex<HashSet<String>>>,
    restarting: Arc<Mutex<HashSet<String>>>,
    alive: Arc<AtomicBool>,
}

/// One restart per agent at a time: held while its pane is replaced.
struct RestartGuard {
    set: Arc<Mutex<HashSet<String>>>,
    agent: String,
}

impl RestartGuard {
    fn claim(set: &Arc<Mutex<HashSet<String>>>, agent: &str) -> Option<Self> {
        set.lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(agent.to_string())
            .then(|| Self { set: Arc::clone(set), agent: agent.to_string() })
    }
}

impl Drop for RestartGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.agent);
    }
}

/// The claude command line of one agent's pane: the program, the agent's
/// permission flags, then `session_args` (its conversation and persona).
fn agent_command(
    spec: &PaneSpawnSpec,
    mode: SwarmTermPermissionMode,
    agent_id: &str,
    session_id: &str,
    session_args: Vec<String>,
) -> String {
    let mut args =
        permissions::claude_permission_args(mode, agent_id, session_id, &spec.facts);
    if let Some(model) = spec.models.get(agent_id) {
        args.extend(["--model".to_string(), model.clone()]);
    }
    args.extend(session_args);
    format!("{} {}", spec.program, permissions::join_quoted(&args))
}

/// How one agent's pane is spawned: in the project, on the pane
/// environment plus the agent's bridge paths.
fn agent_pane_input(
    project_dir: &Path,
    bridgespace_root: &Path,
    agent_id: &str,
    cmd: String,
) -> PaneSpawnInput {
    let mut extra_env = pane_config::pane_env();
    // Bridge paths — informational (the persona prompt carries them),
    // exported so `echo $NEURON_BRIDGE` in a pane shows them while
    // debugging.
    let outbox_self = bridgespace_root.join("outbox").join(agent_id).display().to_string();
    extra_env.insert("NEURON_BRIDGE".to_string(), bridgespace_root.display().to_string());
    extra_env.insert("NEURON_AGENT_ID".to_string(), agent_id.to_string());
    extra_env.insert("NEURON_OUTBOX".to_string(), outbox_self);
    PaneSpawnInput {
        cwd: project_dir.display().to_string(),
        cmd: Some(cmd),
        cols: Some(400),
        rows: Some(30),
        agent_kind: Some("claude-code".into()),
        role: Some(agent_id.to_string()),
        workspace: Some("swarm-term".into()),
        extra_env: Some(extra_env),
    }
}

/// Hold (or release) the bridge's deliveries for `reason`. The stall
/// clocks stand still exactly while the bridge holds, whatever the reasons.
fn hold_deliveries(control: &BridgeControl, lifecycle: &LifecycleStore, reason: PauseReason, on: bool) {
    // A flip can only go the way `on` points: setting a reason pauses,
    // clearing the last one resumes.
    if control.set_paused(reason, on) {
        if on {
            lifecycle.pause_clocks();
        } else {
            lifecycle.resume_clocks();
        }
    }
}

/// `swarm-term:session` with a hold's state change (`paused`, `budget`,
/// `running`) or a grown token count (`tokens`), for the UI.
fn emit_session_state<R: Runtime>(app: &AppHandle<R>, session_id: &str, state: &str) {
    let _ = tauri::Emitter::emit(
        app,
        crate::events::SWARM_TERM_SESSION,
        serde_json::json!({ "sessionId": session_id, "state": state }),
    );
}

/// What the bridge has delivered in this session: one envelope in
/// `processed/<target>/` per agent-to-agent message.
fn count_delivered(bridgespace_root: &Path) -> u32 {
    let Ok(targets) = std::fs::read_dir(bridgespace_root.join("processed")) else {
        return 0;
    };
    targets
        .flatten()
        .filter_map(|target| std::fs::read_dir(target.path()).ok())
        .map(|files| {
            files
                .flatten()
                .filter(|f| f.path().extension().is_some_and(|e| e == "json"))
                .count() as u32
        })
        .sum()
}

/// `swarm-term:session` with the usage limit's state, for the UI's banner.
fn emit_limit<R: Runtime>(app: &AppHandle<R>, session_id: &str, limit: &UsageLimit) {
    let _ = tauri::Emitter::emit(
        app,
        crate::events::SWARM_TERM_SESSION,
        serde_json::json!({
            "sessionId": session_id,
            "state": if limit.limited { "limited" } else { "running" },
            "resetsAt": limit.resets_at,
        }),
    );
}

/// How often the session monitor looks at the panes and the transcripts.
const MONITOR_EVERY: Duration = Duration::from_secs(3);

/// What [`monitor_session`] watches.
struct SessionMonitor<R: Runtime> {
    app: AppHandle<R>,
    registry: TerminalRegistry,
    panes: SharedPanes,
    claude_sessions: BTreeMap<String, String>,
    projects_dirs: Vec<PathBuf>,
    ready: Arc<Mutex<HashSet<String>>>,
    alive: Arc<AtomicBool>,
    publish: Publisher,
    session_id: String,
    control: Arc<BridgeControl>,
    lifecycle: Arc<LifecycleStore>,
    limit: Arc<Mutex<UsageLimit>>,
    /// Agents being restarted: their old pane is killed on purpose.
    restarting: Arc<Mutex<HashSet<String>>>,
    bridgespace_root: PathBuf,
    /// The message budget (`0`: no limit) and the deliveries counted.
    budget: Arc<AtomicU32>,
    delivered: Arc<AtomicU32>,
    /// Tokens per task, charged from the transcripts (see `tokens`).
    tokens: Arc<Mutex<TokenLedger>>,
}

/// Watch the session until it stops:
///
/// * A pane whose claude process ended leaves the delivery targets and
///   shows as exited — the UI offers to restart it.
/// * The message budget: once the bridge has delivered that many
///   agent-to-agent messages, it holds every delivery (one tick late at
///   most) until the user gives the session more.
/// * The account's usage limit, read from the transcripts
///   ([`limit_signal`]): at the first limit error the bridge holds every
///   delivery and the stall clocks stand still. A message pasted then
///   would be spent on a failed turn — or answer claude's limit menu,
///   whose options include paid ones. claude itself continues when the
///   limit resets; the first answer of an agent the limit stopped (or the
///   user's resume) lets the swarm run again.
/// * A context compaction, read from the same transcripts
///   ([`repin::compaction_signal`]): the agent gets its open tasks and
///   rules pasted back ([`repin_after_compaction`]).
/// * Every answer's token usage, read from the same transcripts
///   ([`tokens::usage_signal`]) and charged to the tasks open on that
///   pane: a task past its budget is reported to the coordinator, one at
///   twice the budget is closed ([`act_on_overrun`]).
/// * Every tool call ([`waits::activity_signal`]): evidence the agent is
///   working, which restarts its tasks' stall clocks (and, for a
///   reviewer, its builder's review clock) and feeds the wait graph.
async fn monitor_session<R: Runtime>(m: SessionMonitor<R>) {
    let mut tails: HashMap<String, TranscriptTail> = HashMap::new();
    let mut gone: HashSet<String> = HashSet::new();
    // Compactions whose re-pin is still owed (held during the usage limit);
    // one per agent — the newest is the state that counts.
    let mut compactions: BTreeMap<String, Compaction> = BTreeMap::new();
    // Token budget crossings whose notices are still owed (held likewise).
    let mut overruns: Vec<Overrun> = Vec::new();
    loop {
        tokio::time::sleep(MONITOR_EVERY).await;
        if !m.alive.load(Ordering::SeqCst) {
            return;
        }
        let current = m.panes.read().unwrap_or_else(|p| p.into_inner()).clone();
        for (agent, pane) in &current {
            // The registry drops a pane once its process has ended.
            if gone.contains(pane) || m.registry.pane_status(pane).await.is_some() {
                continue;
            }
            if m.restarting.lock().unwrap_or_else(|p| p.into_inner()).contains(agent) {
                continue;
            }
            gone.insert(pane.clone());
            if let Ok(mut g) = m.ready.lock() {
                g.remove(pane);
            }
            (m.publish)(agent, pane, PaneReadiness::Exited, Some(PANE_EXITED.into()));
            tracing::warn!(agent_id = %agent, pane_id = %pane, "swarm-term: a pane's claude process ended");
        }

        let delivered = count_delivered(&m.bridgespace_root);
        m.delivered.store(delivered, Ordering::SeqCst);
        {
            // Read the budget and the hold state UNDER the limit lock that
            // `extend_budget` also takes: otherwise the monitor could read
            // a stale budget and re-apply the hold the user just cleared,
            // which nothing would then lift.
            let _holds = m.limit.lock().unwrap_or_else(|p| p.into_inner());
            let budget = m.budget.load(Ordering::SeqCst);
            if budget > 0 && delivered >= budget && !m.control.paused_by(PauseReason::Budget) {
                hold_deliveries(&m.control, &m.lifecycle, PauseReason::Budget, true);
                tracing::warn!(delivered, budget, "swarm-term: message budget used up — deliveries held");
                emit_session_state(&m.app, &m.session_id, "budget");
            }
        }

        let mut signals: Vec<(String, LimitSignal)> = Vec::new();
        let tokens_before = m.tokens.lock().unwrap_or_else(|p| p.into_inner()).total();
        for (agent, claude_session) in &m.claude_sessions {
            let Some(file) = pane_config::find_transcript(&m.projects_dirs, claude_session) else {
                continue;
            };
            let lines = tails.entry(agent.clone()).or_default().read_new(&file);
            signals.extend(lines.iter().filter_map(|l| limit_signal(l)).map(|s| (agent.clone(), s)));
            compactions.extend(
                lines.iter().filter_map(|l| repin::compaction_signal(l)).map(|c| (agent.clone(), c)),
            );
            for usage in lines.iter().filter_map(|l| tokens::usage_signal(l)) {
                let open: Vec<String> = current
                    .get(agent)
                    .map(|pane| m.lifecycle.open_tasks_of(pane).into_iter().map(|t| t.id).collect())
                    .unwrap_or_default();
                let crossed =
                    m.tokens.lock().unwrap_or_else(|p| p.into_inner()).observe(agent, &usage, &open);
                overruns.extend(crossed);
            }
            if lines.iter().any(|l| waits::activity_signal(l)) {
                m.lifecycle.note_activity(agent);
                if let Some(pane) = current.get(agent) {
                    m.lifecycle.note_progress(pane);
                }
                // A domain reviewer's work keeps its builder's `review`
                // wait fresh; the tester's own test task is on its own pane.
                if agent != TESTER {
                    for builder in builders_judged_by(agent) {
                        if let Some(pane) = current.get(*builder) {
                            m.lifecycle.note_review_progress(pane);
                        }
                    }
                }
            }
        }
        if m.tokens.lock().unwrap_or_else(|p| p.into_inner()).total() != tokens_before {
            // The UI's token readout re-reads the session on this.
            emit_session_state(&m.app, &m.session_id, "tokens");
        }
        if !signals.is_empty() {
            let mut limit = m.limit.lock().unwrap_or_else(|p| p.into_inner());
            let before = (limit.limited, limit.resets_at);
            for (agent, signal) in signals {
                limit.observe(&agent, signal);
            }
            if (limit.limited, limit.resets_at) != before {
                if limit.limited != before.0 {
                    hold_deliveries(&m.control, &m.lifecycle, PauseReason::UsageLimit, limit.limited);
                    if limit.limited {
                        tracing::warn!(
                            resets_at = ?limit.resets_at,
                            "swarm-term: usage limit hit — deliveries and stall clocks held"
                        );
                    } else {
                        tracing::info!("swarm-term: an agent got through — usage limit over, deliveries resume");
                    }
                }
                emit_limit(&m.app, &m.session_id, &limit);
            }
        }
        // A notice pasted during the usage limit would be spent on a failed
        // turn (or answer claude's limit menu): it waits for the limit to lift.
        if m.control.paused_by(PauseReason::UsageLimit) {
            continue;
        }
        for (agent, compaction) in std::mem::take(&mut compactions) {
            repin_after_compaction(&m, &current, &agent, &compaction).await;
        }
        for overrun in std::mem::take(&mut overruns) {
            act_on_overrun(&m, &current, &overrun).await;
        }
    }
}

/// Paste `agent` the re-pin note after a `compaction` of its context
/// (see [`repin`]): its open tasks with their dispatches, what the
/// coordinator owes, its own rules and limits. Best-effort, like every
/// `[Neuron]` notice; an agent being restarted is skipped (its restart
/// kickoff tells it to go on with its work).
async fn repin_after_compaction<R: Runtime>(
    m: &SessionMonitor<R>,
    panes: &HashMap<String, String>,
    agent: &str,
    compaction: &Compaction,
) {
    let Some(pane) = panes.get(agent) else {
        return;
    };
    if m.restarting.lock().unwrap_or_else(|p| p.into_inner()).contains(agent) {
        return;
    }
    // A compaction is the agent at work (it filled its context doing
    // something) and stops it for a while: neither a stall nor a wait.
    m.lifecycle.note_activity(agent);
    m.lifecycle.note_progress(pane);
    let open = m.lifecycle.open_tasks_of(pane);
    let owed = if agent == "coordinator" {
        m.lifecycle.owed_by_coordinator()
    } else {
        Vec::new()
    };
    let outbox = m.bridgespace_root.join("outbox").join(agent);
    let text = repin::repin_message(agent, compaction, &open, &owed, &outbox);
    tracing::info!(
        agent_id = %agent,
        trigger = %compaction.trigger,
        pre_tokens = compaction.pre_tokens,
        post_tokens = compaction.post_tokens,
        open_tasks = open.len(),
        "swarm-term: context compacted — rules and task state re-pinned"
    );
    bridge::notify_agent(&m.registry, panes, agent, &text).await;
    bridge::emit_notice(&m.app, agent, &text, "context compacted");
}

/// A task crossed its token budget (see [`tokens`]): tell the
/// coordinator; at twice the budget close it — `TASK_FAILED` to the
/// orchestrator through the bridge (which records the closure when it
/// delivers it, as for a stalled task given up), and a word to the
/// agents still holding it.
async fn act_on_overrun<R: Runtime>(
    m: &SessionMonitor<R>,
    panes: &HashMap<String, String>,
    overrun: &Overrun,
) {
    let budget = m.tokens.lock().unwrap_or_else(|p| p.into_inner()).budget();
    let id = overrun.task_id.as_str();
    match overrun.crossing {
        Crossing::Warn => {
            let text = tokens::warn_notice(id, overrun.spent, budget);
            tracing::warn!(
                task_id = %id,
                spent = overrun.spent,
                budget,
                "swarm-term: task past its token budget — coordinator told"
            );
            bridge::notify_agent(&m.registry, panes, "coordinator", &text).await;
            bridge::emit_notice(&m.app, "coordinator", &text, "task past its token budget");
        }
        Crossing::Stop => {
            let (failed, coordinator, holder) = tokens::stop_texts(id, overrun.spent, budget);
            tracing::warn!(
                task_id = %id,
                spent = overrun.spent,
                budget,
                "swarm-term: task at twice its token budget — closed, TASK_FAILED"
            );
            // Who holds it, before the closure settles their entries.
            let holders: Vec<String> = panes
                .iter()
                .filter(|(_, pane)| m.lifecycle.open_tasks_of(pane).iter().any(|t| t.id == id))
                .map(|(agent, _)| agent.clone())
                .collect();
            // Closed here and now: the UI hears it at once, and the
            // TASK_FAILED below finds the task closed when the bridge
            // delivers it (live run 2: the delivery had recorded a second
            // "coordinator gave up" card for the same stop).
            if m.lifecycle.fail_task(id) {
                let pane = panes.get("coordinator").map(String::as_str).unwrap_or("");
                let owners = bridge::task_owners(&m.lifecycle, panes, id);
                bridge::emit_lifecycle(&m.app, "coordinator", pane, id, "TaskFailed", LifecycleState::Failed, &owners);
            }
            bridge::enqueue_fanout(&m.app, &m.bridgespace_root, panes, "orchestrator", failed, id);
            experience::record_for_session(
                &m.bridgespace_root,
                experience::Kind::Failed,
                &format!(
                    "{id} — {} token bütçesinin iki katı harcandı ({})",
                    crate::swarm_term::mast::TOKEN_OVERRUN.tag(),
                    tokens::fmt_tokens(overrun.spent)
                ),
            );
            bridge::notify_agent(&m.registry, panes, "coordinator", &coordinator).await;
            bridge::emit_notice(&m.app, "coordinator", &coordinator, "task at twice its token budget");
            for agent in &holders {
                bridge::notify_agent(&m.registry, panes, agent, &holder).await;
                bridge::emit_notice(&m.app, agent, &holder, "task at twice its token budget");
            }
        }
    }
}

/// What one transcript line says about the account's usage limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LimitSignal {
    /// The limit stopped this turn; `resets_at` (unix seconds) when
    /// claude said.
    Hit { resets_at: Option<i64> },
    /// The model answered: the account works.
    Working,
}

/// Pure: read one transcript line for a [`LimitSignal`]. claude records
/// an API error as an `assistant` entry of the `<synthetic>` model with
/// `isApiErrorMessage`; the usage limit is its `rate_limit` (HTTP 429)
/// kind, with `quotaLimits.resetsAt` when known. Other errors (overload,
/// login) say nothing about the limit.
fn limit_signal(line: &str) -> Option<LimitSignal> {
    use serde_json::Value;
    // Cheap pre-check: most lines are user turns and tool results.
    if !line.contains("\"assistant\"") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    if v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true) {
        let limited = v.get("error").and_then(Value::as_str) == Some("rate_limit")
            || v.get("apiErrorStatus").and_then(Value::as_u64) == Some(429);
        return limited.then(|| LimitSignal::Hit {
            resets_at: v.pointer("/quotaLimits/resetsAt").and_then(Value::as_i64),
        });
    }
    // claude's own notices are synthetic, too: no word from the API.
    if v.pointer("/message/model").and_then(Value::as_str) == Some("<synthetic>") {
        return None;
    }
    Some(LimitSignal::Working)
}

/// The account's usage limit as the session tracks it.
#[derive(Debug, Default)]
pub(crate) struct UsageLimit {
    /// Hit: the bridge holds every delivery, the stall clocks stand still.
    limited: bool,
    /// When claude said the limit resets (unix seconds).
    resets_at: Option<i64>,
    /// Agents whose newest transcript word is the limit error.
    hit: BTreeSet<String>,
}

impl UsageLimit {
    /// Fold in one of `agent`'s transcript signals.
    fn observe(&mut self, agent: &str, signal: LimitSignal) {
        match signal {
            LimitSignal::Hit { resets_at } => {
                self.limited = true;
                self.hit.insert(agent.to_string());
                if resets_at.is_some() {
                    self.resets_at = resets_at;
                }
            }
            // Only an agent the limit stopped proves it is over: a turn
            // another pane had in flight may still finish after the hit.
            LimitSignal::Working => {
                if self.hit.remove(agent) && self.limited {
                    self.clear();
                }
            }
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Reads a growing transcript's new complete lines, tick by tick.
#[derive(Debug, Default)]
struct TranscriptTail {
    offset: u64,
    /// The start of a line claude has not finished writing.
    partial: Vec<u8>,
}

impl TranscriptTail {
    fn read_new(&mut self, file: &Path) -> Vec<String> {
        let Ok(mut f) = std::fs::File::open(file) else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // Replaced or truncated: read it from the start.
            *self = Self::default();
        }
        if len == self.offset || f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        let Ok(read) = f.take(len - self.offset).read_to_end(&mut buf) else {
            return Vec::new();
        };
        self.offset += read as u64;
        self.partial.extend_from_slice(&buf);
        let mut lines = Vec::new();
        while let Some(end) = self.partial.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            lines.push(String::from_utf8_lossy(&line).trim_end().to_string());
        }
        lines
    }
}

/// Delete a stopped session's transcripts and Neuron files, retrying while
/// the just-killed claude processes still hold files open (os error 32 on
/// Windows). What is still left after the last attempt goes in the next
/// start's orphan sweep.
async fn remove_session_with_retry(projects_dirs: &[PathBuf], session_dir: &Path) {
    for attempt in 1..=CLEANUP_ATTEMPTS {
        let left = pane_config::remove_session_files(projects_dirs, session_dir);
        if left.is_empty() {
            tracing::info!(
                path = %session_dir.display(),
                attempt,
                "swarm-term: session transcripts and files cleaned up"
            );
            return;
        }
        if attempt == CLEANUP_ATTEMPTS {
            tracing::warn!(
                path = %session_dir.display(),
                left = ?left,
                attempts = CLEANUP_ATTEMPTS,
                "swarm-term: session cleanup incomplete; the next start sweeps it"
            );
            return;
        }
        tokio::time::sleep(CLEANUP_RETRY_DELAY).await;
    }
}

/// Undo a `start()` that failed part-way: kill the panes spawned so far
/// and delete what the session put on disk.
async fn abandon_start(
    registry: &TerminalRegistry,
    pool: &DbPool,
    spawned: &[String],
    projects_dirs: &[PathBuf],
    session_dir: &Path,
    bridgespace_root: &Path,
) {
    for pid in spawned {
        let _ = registry.kill_pane(pid, pool).await;
    }
    remove_session_with_retry(projects_dirs, session_dir).await;
    let _ = std::fs::remove_dir_all(bridgespace_root);
}

/// Lower-case fragments of claude's folder-trust screen — one of the
/// blocking screens below, and the one the session answers for the user:
/// they trusted the folder in Neuron before the session could start.
const TRUST_SCREEN_MARKERS: &[&str] = &[
    "trust the files",
    "is this a project you created or one you trust",
    "yes, i trust this folder",
];

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
    // claude's usage-limit menu ("What do you want to do?"): Enter picks
    // one of its options — some of them paid.
    "stop and wait for limit to reset",
    "wait here, then continue automatically",
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

/// Whitespace removed, lower-cased: how screens and markers are compared
/// (see [`BLOCKING_SCREEN_MARKERS`]).
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase()
}

fn screen_is_blocking(screen_tail: &str) -> bool {
    let screen = squash(screen_tail);
    BLOCKING_SCREEN_MARKERS.iter().any(|m| screen.contains(&squash(m)))
}

/// The keys that pick "Yes, I trust this folder" on claude's trust screen
/// and confirm it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustKeys {
    /// The cursor is on "Yes" already.
    Confirm,
    /// "Yes" is below the cursor.
    DownConfirm,
    /// "Yes" is above the cursor.
    UpConfirm,
}

/// Pure: how to answer the trust screen, read from where the menu cursor
/// (`❯`) is. `None` when the screen is not that menu or the cursor cannot
/// be placed — then nothing is sent, because Enter on the default
/// "No, exit" quits the pane's claude.
fn trust_screen_keys(screen_tail: &str) -> Option<TrustKeys> {
    const YES: &str = "yes,itrustthisfolder";
    const NO: &str = "no,exit";
    let screen = squash(screen_tail);
    if !TRUST_SCREEN_MARKERS.iter().any(|m| screen.contains(&squash(m))) {
        return None;
    }
    // The last cursor on screen is the live menu; an option may carry a
    // number ("❯ 1. Yes, …").
    let cursor = screen.rfind('❯')?;
    let at_cursor = screen[cursor + '❯'.len_utf8()..]
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    if at_cursor.starts_with(YES) {
        return Some(TrustKeys::Confirm);
    }
    if !at_cursor.starts_with(NO) {
        return None;
    }
    if screen[cursor..].contains(YES) {
        Some(TrustKeys::DownConfirm)
    } else if screen[..cursor].contains(YES) {
        Some(TrustKeys::UpConfirm)
    } else {
        None
    }
}

/// Answer the trust screen in one pane with `keys` (best-effort, logged).
async fn press_trust_keys(registry: &TerminalRegistry, agent_id: &str, pane_id: &str, keys: TrustKeys) {
    let arrow: Option<&[u8]> = match keys {
        TrustKeys::Confirm => None,
        TrustKeys::DownConfirm => Some(b"\x1b[B"),
        TrustKeys::UpConfirm => Some(b"\x1b[A"),
    };
    if let Some(arrow) = arrow {
        if let Err(e) = registry.write_to_pane(pane_id, arrow).await {
            tracing::warn!(agent_id = %agent_id, error = %e, "swarm-term: trust answer failed");
            return;
        }
        // Let the menu move its cursor before Enter confirms.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    match registry.write_to_pane(pane_id, b"\r").await {
        Ok(()) => tracing::info!(
            agent_id = %agent_id,
            keys = ?keys,
            "swarm-term: answered the folder-trust screen (the user trusted the folder in Neuron)"
        ),
        Err(e) => tracing::warn!(agent_id = %agent_id, error = %e, "swarm-term: trust answer failed"),
    }
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

/// Paste a kickoff turn (`kickoff(agent_id)`) into one pane (best-effort,
/// logged) — unless the pane is on a login / trust / first-run screen,
/// where it is withheld; `wait_for_ready_answers` sends it once the user
/// resolves the screen.
///
/// Returns whether the kickoff was pasted (`false` = withheld).
async fn send_kickoff(
    registry: &TerminalRegistry,
    pool: &DbPool,
    agent_id: &str,
    pane_id: &str,
    kickoff: fn(&str) -> String,
) -> bool {
    if pane_on_blocking_screen(registry, pool, pane_id).await {
        tracing::warn!(
            agent_id = %agent_id,
            "swarm-term: kickoff withheld — pane is on a login / trust / \
             first-run screen; it is sent once the screen is resolved"
        );
        return false;
    }
    let kickoff = kickoff(agent_id);
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

/// Records a pane's readiness — called `(agent, pane, readiness, detail)`
/// — and, when it changed, emits `swarm-term:pane-state` (see
/// `pane_state`). Only for the agent's CURRENT pane: once an agent is
/// restarted, whatever still watches its old pane has nothing left to say.
type Publisher = Arc<dyn Fn(&str, &str, PaneReadiness, Option<String>) + Send + Sync>;

fn pane_publisher<R: Runtime>(
    app: AppHandle<R>,
    panes: SharedPanes,
    states: Arc<PaneStates>,
    session_id: String,
    alive: Arc<AtomicBool>,
) -> Publisher {
    Arc::new(move |agent: &str, pane: &str, readiness: PaneReadiness, detail: Option<String>| {
        // A stopped session's panes are killed on purpose: nothing to say.
        if !alive.load(Ordering::SeqCst) {
            return;
        }
        // Checked and recorded under the map's read lock: a restart swaps
        // the pane under the write lock, so an old pane's last word can
        // never land after the new pane's first.
        let current = panes.read().unwrap_or_else(|p| p.into_inner());
        if current.get(agent).map(String::as_str) != Some(pane) {
            return;
        }
        if states.set(agent, readiness, detail.clone()) {
            let _ = tauri::Emitter::emit(
                &app,
                crate::events::SWARM_TERM_PANE_STATE,
                serde_json::json!({
                    "sessionId": session_id,
                    "agentId": agent,
                    "readiness": readiness,
                    "detail": detail,
                }),
            );
        }
    })
}

/// Detail for a pane whose claude process ended.
const PANE_EXITED: &str = "Pane'in claude süreci kapandı.";

/// Detail for a pane stuck on claude's folder-trust screen that the
/// session's auto-answer could not clear.
const TRUST_UNANSWERED: &str =
    "Claude'un güven ekranı otomatik yanıtlanamadı: pane'e tıklayıp \"Yes, I trust this folder\" seç.";

/// How often the panes that gave no answer in time are re-checked.
const LATE_ANSWER_POLL: Duration = Duration::from_secs(5);

/// What a readiness wait needs — owned, so a wait runs in its own task.
#[derive(Clone)]
struct ReadyWait {
    registry: TerminalRegistry,
    pool: DbPool,
    /// The current agent → pane map: an agent restarted meanwhile is left
    /// to its new pane's wait.
    panes: SharedPanes,
    claude_sessions: BTreeMap<String, String>,
    projects_dirs: Vec<PathBuf>,
    ready: Arc<Mutex<HashSet<String>>>,
    alive: Arc<AtomicBool>,
    /// Agents whose pane is being replaced right now (see
    /// [`TerminalSwarmRegistry::restart_agent`]): a wait on the old pane
    /// must not report it Exited in that window.
    restarting: Arc<Mutex<HashSet<String>>>,
    publish: Publisher,
}

impl ReadyWait {
    /// Is `pane` still `agent`'s (not replaced by a restart)?
    fn is_current(&self, agent: &str, pane: &str) -> bool {
        self.panes.read().unwrap_or_else(|p| p.into_inner()).get(agent).map(String::as_str)
            == Some(pane)
    }

    fn is_restarting(&self, agent: &str) -> bool {
        self.restarting.lock().map(|g| g.contains(agent)).unwrap_or(false)
    }
}

/// A pane a readiness wait waits on, and from where in its agent's
/// transcript an answer counts: a restarted agent resumes its
/// conversation, and the `READY` it gave before must not count again.
#[derive(Debug, Clone)]
struct Awaited {
    pane: String,
    from_byte: u64,
}

/// Send each awaited pane its kickoff and poll its agent's claude
/// transcript until it has answered, marking it ready as soon as it does.
///
/// * An unsent kickoff goes out once the pane's screen has settled on
///   something other than a login / trust / first-run screen
///   ([`ScreenSettle`]) — so it can never answer such a screen, and a
///   screen the user resolves late still gets its kickoff. Watched for
///   up to [`BLOCKED_SCREEN_WAIT`].
/// * claude's folder-trust screen is answered for the user, who trusted
///   the folder in Neuron before the session could start
///   ([`trust_screen_keys`]; at most [`TRUST_ANSWER_ATTEMPTS`] times).
/// * A sent kickoff without an answer is re-sent once after
///   [`KICKOFF_RESEND_AFTER`] (it may have landed before the REPL
///   finished booting) and given up (warn, pane stays not-ready)
///   [`READY_TIMEOUT`] after it was sent.
///
/// Returns the agents that gave no answer in time (for
/// [`watch_late_answers`]) as soon as every pane is settled or the
/// session is stopped (`alive` cleared). An agent restarted meanwhile is
/// left to its new pane's wait.
async fn wait_for_ready_answers(
    w: &ReadyWait,
    awaited: &HashMap<String, Awaited>,
    kickoff: fn(&str) -> String,
) -> Vec<String> {
    let started = Instant::now();
    let mut pending: HashSet<&str> = awaited.keys().map(String::as_str).collect();
    // When each pane's kickoff was sent (absent = not yet).
    let mut kicked: HashMap<String, Instant> = HashMap::new();
    let mut late: Vec<String> = Vec::new();
    let mut resent: HashSet<String> = HashSet::new();
    let mut settle: HashMap<String, ScreenSettle> = HashMap::new();
    let mut trust_answers: HashMap<String, (u32, Instant)> = HashMap::new();
    let mut last_screen_check = Instant::now() - BLOCKED_SCREEN_POLL;
    loop {
        if !w.alive.load(Ordering::SeqCst) {
            return late;
        }
        pending.retain(|agent| w.is_current(agent, &awaited[*agent].pane));
        let answered: Vec<(&str, bool)> = pending
            .iter()
            .copied()
            // Only a pane whose kickoff is out can have answered it, so a
            // pane still settling on a screen needs no transcript lookup.
            .filter(|agent| kicked.contains_key(*agent))
            .filter_map(|agent| {
                let claude_session = w.claude_sessions.get(agent)?;
                kickoff_answer(&w.projects_dirs, claude_session, agent, awaited[agent].from_byte)
                    .map(|exact| (agent, exact))
            })
            .collect();
        for (agent, exact) in answered {
            pending.remove(agent);
            let pane = &awaited[agent].pane;
            if let Ok(mut g) = w.ready.lock() {
                g.insert(pane.clone());
            }
            (w.publish)(agent, pane, PaneReadiness::Ready, None);
            tracing::info!(
                agent_id = %agent,
                exact,
                waited_ms = started.elapsed().as_millis() as u64,
                "swarm-term: pane answered kickoff — ready"
            );
        }

        let check_screens = last_screen_check.elapsed() >= BLOCKED_SCREEN_POLL;
        if check_screens {
            last_screen_check = Instant::now();
        }
        let mut gave_up: Vec<&str> = Vec::new();
        let mut exited: Vec<&str> = Vec::new();
        for &agent in &pending {
            let pane = awaited[agent].pane.as_str();
            // The registry drops a pane once its process has ended.
            if check_screens && w.registry.pane_status(pane).await.is_none() {
                // A restart kills the old pane on purpose: leave it to the
                // swap (then `is_current` drops it), don't report Exited.
                if w.is_restarting(agent) {
                    continue;
                }
                (w.publish)(agent, pane, PaneReadiness::Exited, Some(PANE_EXITED.into()));
                exited.push(agent);
                continue;
            }
            match kicked.get(agent).copied() {
                // Not sent yet: send it once the screen has settled on
                // something that is not a login / trust screen.
                None => {
                    if started.elapsed() >= BLOCKED_SCREEN_WAIT {
                        (w.publish)(
                            agent,
                            pane,
                            PaneReadiness::NotReady,
                            Some(format!(
                                "Pane {} dakika boyunca bir ekranda bekledi.",
                                BLOCKED_SCREEN_WAIT.as_secs() / 60
                            )),
                        );
                        gave_up.push(agent);
                    } else if check_screens {
                        let screen = pane_screen_tail(&w.registry, &w.pool, pane).await.unwrap_or_default();
                        if let Some(keys) = trust_screen_keys(&screen) {
                            let (tries, last) = trust_answers
                                .get(agent)
                                .copied()
                                .unwrap_or((0, started - TRUST_ANSWER_EVERY));
                            if tries < TRUST_ANSWER_ATTEMPTS && last.elapsed() >= TRUST_ANSWER_EVERY {
                                press_trust_keys(&w.registry, agent, pane, keys).await;
                                trust_answers.insert(agent.to_string(), (tries + 1, Instant::now()));
                            } else if tries >= TRUST_ANSWER_ATTEMPTS {
                                // The auto-answer didn't clear it: say so, instead
                                // of sitting on "starting" until the 30-min give-up.
                                (w.publish)(agent, pane, PaneReadiness::Blocked, Some(TRUST_UNANSWERED.into()));
                            }
                            continue;
                        }
                        if screen_is_blocking(&screen) {
                            (w.publish)(agent, pane, PaneReadiness::Blocked, Some(blocked_reason(&screen).into()));
                        } else {
                            (w.publish)(agent, pane, PaneReadiness::Starting, None);
                        }
                        let settled = settle.entry(agent.to_string()).or_default().observe(&screen);
                        let fallback = started.elapsed() >= SETTLE_FALLBACK
                            && !screen.trim().is_empty()
                            && !screen_is_blocking(&screen);
                        if (settled || fallback)
                            && send_kickoff(&w.registry, &w.pool, agent, pane, kickoff).await
                        {
                            tracing::info!(
                                agent_id = %agent,
                                waited_ms = started.elapsed().as_millis() as u64,
                                "swarm-term: pane screen settled — kickoff sent"
                            );
                            kicked.insert(agent.to_string(), Instant::now());
                            (w.publish)(agent, pane, PaneReadiness::KickoffSent, None);
                        }
                    }
                }
                Some(sent_at) => {
                    let since = sent_at.elapsed();
                    if since >= READY_TIMEOUT {
                        (w.publish)(
                            agent,
                            pane,
                            PaneReadiness::NotReady,
                            Some(format!(
                                "Kickoff'a {} sn içinde yanıt vermedi; ona giden mesajlar \
                                 gönderene iade ediliyor. Yanıt verirse hazır sayılır.",
                                READY_TIMEOUT.as_secs()
                            )),
                        );
                        gave_up.push(agent);
                    } else if since >= KICKOFF_RESEND_AFTER && resent.insert(agent.to_string()) {
                        tracing::warn!(
                            agent_id = %agent,
                            "swarm-term: no kickoff answer yet — re-sending once"
                        );
                        if !send_kickoff(&w.registry, &w.pool, agent, pane, kickoff).await {
                            // A blocking screen appeared: wait for it again.
                            kicked.remove(agent);
                        }
                    }
                }
            }
        }
        for agent in exited {
            pending.remove(agent);
            tracing::warn!(agent_id = %agent, "swarm-term: pane exited before it was ready");
        }
        for agent in gave_up {
            pending.remove(agent);
            late.push(agent.to_string());
            tracing::warn!(
                agent_id = %agent,
                "swarm-term: pane never answered the kickoff; not ready for now \
                 (messages to it are returned to the sender; a late answer still counts)"
            );
        }
        // Keep the already-late panes polled WHILE others are still
        // pending: one pane stuck on a screen must not hold a late READY
        // from another unseen for up to BLOCKED_SCREEN_WAIT.
        late = check_late_once(w, awaited, late).await;
        if pending.is_empty() {
            return late;
        }
        tokio::time::sleep(READY_POLL).await;
    }
}

/// Keep watching the panes that gave no answer in time: a late `READY`
/// still makes a pane a delivery target (it used to stay not-ready for
/// the whole session), and an exit is reported. Ends with the session,
/// or once none is left.
async fn watch_late_answers(w: &ReadyWait, awaited: &HashMap<String, Awaited>, mut late: Vec<String>) {
    while !late.is_empty() && w.alive.load(Ordering::SeqCst) {
        tokio::time::sleep(LATE_ANSWER_POLL).await;
        if !w.alive.load(Ordering::SeqCst) {
            return;
        }
        late = check_late_once(w, awaited, late).await;
    }
}

/// One pass over the panes that missed their kickoff deadline: a late
/// `READY` makes a still-live pane a delivery target; a pane that has
/// since exited is dropped (the session monitor reports the exit — a
/// late answer must not resurrect it). An agent being restarted, or
/// whose pane was replaced, is kept for its new pane's wait. Returns the
/// ones still waiting.
async fn check_late_once(
    w: &ReadyWait,
    awaited: &HashMap<String, Awaited>,
    late: Vec<String>,
) -> Vec<String> {
    let mut still: Vec<String> = Vec::new();
    for agent in late {
        let Some(Awaited { pane, from_byte }) = awaited.get(&agent) else {
            continue;
        };
        // Restarted meanwhile: the new pane's wait has it.
        if !w.is_current(&agent, pane) {
            continue;
        }
        let answered = w.claude_sessions.get(&agent).is_some_and(|id| {
            kickoff_answer(&w.projects_dirs, id, &agent, *from_byte).is_some()
        });
        let alive = w.registry.pane_status(pane).await.is_some();
        if answered && alive {
            if let Ok(mut g) = w.ready.lock() {
                g.insert(pane.clone());
            }
            (w.publish)(&agent, pane, PaneReadiness::Ready, None);
            tracing::info!(agent_id = %agent, "swarm-term: pane answered the kickoff late — ready");
        } else if !alive {
            // A restart kills the old pane on purpose — keep watching for
            // the new one; otherwise the exit is the monitor's to report.
            if w.is_restarting(&agent) {
                still.push(agent);
            }
        } else {
            still.push(agent);
        }
    }
    still
}

/// Pure: what a blocking (non-trust) screen asks of the user, for the UI.
fn blocked_reason(screen_tail: &str) -> &'static str {
    let screen = squash(screen_tail);
    let has = |m: &str| screen.contains(&squash(m));
    if has("select login method") || has("please run /login") {
        "Claude giriş istiyor: bu pane'de giriş yap ya da bir terminalde `claude` ile `/login` yap."
    } else if has("choose the text style") {
        "Claude'un ilk açılış ekranı açık: pane'e tıklayıp yanıtla."
    } else if has("stop and wait for limit to reset") || has("wait here, then continue automatically") {
        "Claude'un kullanım limiti menüsü açık: pane'e tıklayıp seç. \"Stop and wait…\" ve \
         \"Wait here…\" ücret çıkarmaz."
    } else {
        "Pane bir onay ekranında bekliyor: pane'e tıklayıp yanıtla."
    }
}

/// How the pane's claude has answered its kickoff, read from its
/// transcript (the one named by its `--session-id`) at or after byte
/// `from_byte` — a restarted pane's resumed transcript holds its earlier
/// answer. `None`: no answer yet; `Some(exact)`: the model answered, in
/// the requested `@<id> READY` form when `exact`. Any answer of the model
/// counts — in its own pane's transcript it can only be this pane's, and
/// a model that words it otherwise (claude-haiku answers a bare `READY`)
/// has answered all the same. claude's own notices and API errors are no
/// answer.
fn kickoff_answer(
    projects_dirs: &[PathBuf],
    claude_session: &str,
    agent_id: &str,
    from_byte: u64,
) -> Option<bool> {
    let file = pane_config::find_transcript(projects_dirs, claude_session)?;
    let mut f = std::fs::File::open(file).ok()?;
    let mut bytes = Vec::new();
    f.seek(SeekFrom::Start(from_byte)).ok()?;
    f.read_to_end(&mut bytes).ok()?;
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| line_answer(l, agent_id))
        .reduce(|a, b| a || b)
}

/// Pure: one transcript line as a kickoff answer (see [`kickoff_answer`]).
fn line_answer(line: &str, agent_id: &str) -> Option<bool> {
    use serde_json::Value;
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant")
        || v.get("isApiErrorMessage").and_then(Value::as_bool) == Some(true)
        || v.pointer("/message/model").and_then(Value::as_str) == Some("<synthetic>")
    {
        return None;
    }
    Some(match v.pointer("/message/content") {
        Some(Value::String(s)) => is_ready_answer(agent_id, s),
        Some(Value::Array(parts)) => parts.iter().any(|p| {
            p.get("type").and_then(Value::as_str) == Some("text")
                && p.get("text").and_then(Value::as_str).is_some_and(|t| is_ready_answer(agent_id, t))
        }),
        _ => false,
    })
}

fn handle_from(s: &ActiveSession) -> TerminalSwarmSessionHandle {
    let mut panes: Vec<TerminalSwarmPane> = s
        .panes
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .map(|(agent_id, pane_id)| TerminalSwarmPane {
            agent_id: agent_id.clone(),
            pane_id: pane_id.clone(),
        })
        .collect();
    panes.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    let limit = s.limit.lock().unwrap_or_else(|p| p.into_inner());
    let tokens = s.tokens.lock().unwrap_or_else(|p| p.into_inner());
    TerminalSwarmSessionHandle {
        session_id: s.session_id.clone(),
        project_dir: s.project_dir.display().to_string(),
        panes,
        permission_mode: s.permission_mode,
        claude_version: s.claude_version.clone(),
        preflight_note: s.preflight_note.clone(),
        pane_states: s.pane_states.snapshot(),
        usage_limited: limit.limited,
        limit_resets_at: limit.resets_at,
        changes_note: s.changes_note.clone(),
        lite_team: s.lite_team,
        paused: s.control.paused_by(PauseReason::User),
        messages_delivered: s.delivered.load(Ordering::SeqCst),
        message_budget: s.budget.load(Ordering::SeqCst),
        budget_reached: s.control.paused_by(PauseReason::Budget),
        tokens_used: tokens.total(),
        task_token_budget: tokens.budget(),
    }
}


#[cfg(test)]
pub(crate) mod live;

#[cfg(test)]
mod tests {
    use super::*;

    const READY_LINE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"@scout READY"}]}}"#;
    const KICKOFF_ECHO: &str = r#"{"type":"user","message":{"role":"user","content":"[Neuron] Oturum başladı. @scout READY"}}"#;

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
            // claude's usage-limit menu: Enter would pick an option.
            "What do you want to do?\n❯ 1. Stop and wait for limit to reset\n  2. Upgrade your plan",
            "Wait here, then continue automatically at 4:50pm",
        ] {
            assert!(screen_is_blocking(screen), "{screen}");
        }
        // The ordinary REPL welcome / prompt is not blocking.
        assert!(!screen_is_blocking("✻ Welcome to Claude Code!\n> "));
        assert!(!screen_is_blocking(""));
    }

    /// claude-haiku's kickoff answer, as a live Lite run wrote it.
    const BARE_READY: &str = r#"{"type":"assistant","message":{"model":"claude-haiku-5-5","role":"assistant","content":[{"type":"text","text":"READY"}]}}"#;

    #[test]
    fn any_answer_of_the_model_makes_a_pane_ready() {
        assert_eq!(line_answer(READY_LINE, "scout"), Some(true));
        // Another wording is an answer all the same — just not the exact one.
        assert_eq!(line_answer(BARE_READY, "orchestrator"), Some(false));
        assert_eq!(line_answer(ANSWER, "scout"), Some(false));
        // The kickoff echo, claude's own notices and API errors are not.
        assert_eq!(line_answer(KICKOFF_ECHO, "scout"), None);
        assert_eq!(line_answer(LIMIT_HIT, "scout"), None);
        assert_eq!(line_answer(LOGGED_OUT, "scout"), None);
        assert_eq!(line_answer("not json", "scout"), None);
    }

    #[test]
    fn the_ready_answer_is_read_from_the_panes_own_transcript_only() {
        let base = tempfile::tempdir().expect("tempdir");
        let projects = base.path().join("projects");
        let dirs = vec![projects.clone()];
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", 0), None);
        let dir = projects.join("C--proj");
        std::fs::create_dir_all(&dir).unwrap();
        // Another session in the same project — e.g. the user's own —
        // saying the phrase does not make this pane ready.
        std::fs::write(dir.join("theirs.jsonl"), format!("{READY_LINE}\n")).unwrap();
        let mine = dir.join("mine.jsonl");
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n")).unwrap();
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", 0), None);
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n{READY_LINE}\n")).unwrap();
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", 0), Some(true));
        // A restarted pane resumes the conversation: only an answer after
        // the restart counts.
        let restart_at = std::fs::metadata(&mine).unwrap().len();
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", restart_at), None);
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n{READY_LINE}\n{KICKOFF_ECHO}\n")).unwrap();
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", restart_at), None);
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n{READY_LINE}\n{KICKOFF_ECHO}\n{BARE_READY}\n"))
            .unwrap();
        assert_eq!(kickoff_answer(&dirs, "mine", "scout", restart_at), Some(false));
    }

    /// claude's API-error entries, as 2.1.296 writes them (trimmed).
    const LIMIT_HIT: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","apiErrorStatus":429,"quotaLimits":{"status":"rejected","resetsAt":1791899400,"rateLimitType":"five_hour"},"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You've hit your session limit · resets 4:50pm"}]}}"#;
    const LIMIT_HIT_NO_RESET: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","apiErrorStatus":429,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"You're out of extra usage · resets 4:50pm"}]}}"#;
    const OVERLOADED: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"server_error","apiErrorStatus":529,"message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"API Error: 529 Overloaded."}]}}"#;
    const LOGGED_OUT: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"authentication_failed","message":{"model":"<synthetic>","role":"assistant","content":[{"type":"text","text":"Not logged in · Please run /login"}]}}"#;
    const ANSWER: &str = r#"{"type":"assistant","message":{"model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"tamam"}]}}"#;

    #[test]
    fn the_usage_limit_is_read_from_claudes_api_error_entries() {
        assert_eq!(limit_signal(LIMIT_HIT), Some(LimitSignal::Hit { resets_at: Some(1_791_899_400) }));
        assert_eq!(limit_signal(LIMIT_HIT_NO_RESET), Some(LimitSignal::Hit { resets_at: None }));
        assert_eq!(limit_signal(ANSWER), Some(LimitSignal::Working));
        assert_eq!(limit_signal(READY_LINE), Some(LimitSignal::Working));
        // Other API errors, user turns and junk say nothing about it.
        assert_eq!(limit_signal(OVERLOADED), None);
        assert_eq!(limit_signal(LOGGED_OUT), None);
        assert_eq!(limit_signal(KICKOFF_ECHO), None);
        assert_eq!(limit_signal("not json"), None);
    }

    #[test]
    fn only_an_agent_the_limit_stopped_ends_it() {
        let mut l = UsageLimit::default();
        l.observe("builder", LimitSignal::Hit { resets_at: Some(100) });
        assert!(l.limited);
        assert_eq!(l.resets_at, Some(100));
        // A turn another pane had in flight finishes: still limited.
        l.observe("reviewer", LimitSignal::Working);
        assert!(l.limited);
        // A later hit without a time keeps the known one; with one, it wins.
        l.observe("reviewer", LimitSignal::Hit { resets_at: None });
        assert_eq!(l.resets_at, Some(100));
        l.observe("reviewer", LimitSignal::Hit { resets_at: Some(200) });
        assert_eq!(l.resets_at, Some(200));
        // The builder gets through: the limit is over for everyone.
        l.observe("builder", LimitSignal::Working);
        assert!(!l.limited);
        assert_eq!(l.resets_at, None);
        // An answer with no limit around changes nothing.
        l.observe("reviewer", LimitSignal::Working);
        assert!(!l.limited);
    }

    #[test]
    fn the_transcript_tail_reads_each_complete_line_once() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("t.jsonl");
        let mut tail = TranscriptTail::default();
        // Not written yet.
        assert!(tail.read_new(&file).is_empty());
        let mut f = std::fs::File::create(&file).unwrap();
        write!(f, "one\ntw").unwrap();
        f.flush().unwrap();
        assert_eq!(tail.read_new(&file), ["one"]);
        // A half-written line waits for its end.
        assert!(tail.read_new(&file).is_empty());
        write!(f, "o\r\nthree\n").unwrap();
        f.flush().unwrap();
        assert_eq!(tail.read_new(&file), ["two", "three"]);
        assert!(tail.read_new(&file).is_empty());
        drop(f);
        // Replaced by a shorter file: read from the start again.
        std::fs::write(&file, "new\n").unwrap();
        assert_eq!(tail.read_new(&file), ["new"]);
    }

    #[test]
    fn blocked_screens_say_what_the_user_must_do() {
        assert!(blocked_reason("Select login method:
❯ 1. Claude account").contains("/login"));
        assert!(blocked_reason("OAuth token has expired. Please run /login").contains("giriş"));
        assert!(blocked_reason("Choose the text style that looks best").contains("ilk açılış"));
        assert!(blocked_reason("Press Enter to continue").contains("onay ekranında"));
        assert!(blocked_reason("What do you want to do?\n❯ 1. Stop and wait for limit to reset")
            .contains("limiti menüsü"));
    }

    #[test]
    fn the_trust_screen_is_answered_from_where_the_cursor_is() {
        // claude 2.1.29x, as the pane renderer captured it: cursor on the
        // default "No, exit", "Yes" below it.
        assert_eq!(
            trust_screen_keys(
                "Accessingworkspace:C:\\x\\inv Quicksafetycheck:Isthisaprojectyoucreatedoroneyoutrust?\n\
                 ❯ No, exit\n  Yes, I trust this folder\nEnter to confirm"
            ),
            Some(TrustKeys::DownConfirm)
        );
        assert_eq!(
            trust_screen_keys("Do you trust the files in this folder?\n❯ 1. Yes, I trust this folder\n  2. No, exit"),
            Some(TrustKeys::Confirm)
        );
        assert_eq!(
            trust_screen_keys("Do you trust the files in this folder?\n  1. Yes, I trust this folder\n❯ 2. No, exit"),
            Some(TrustKeys::UpConfirm)
        );
        // The last cursor is the live menu (an earlier frame may linger).
        assert_eq!(
            trust_screen_keys(
                "❯ No, exit\n  Yes, I trust this folder\n…\nDo you trust the files in this folder?\n❯ Yes, I trust this folder\n  No, exit"
            ),
            Some(TrustKeys::Confirm)
        );
        // Not the trust menu, or no cursor to go by: nothing is pressed.
        assert_eq!(trust_screen_keys("Select login method:\n❯ 1. Claude account with subscription"), None);
        assert_eq!(trust_screen_keys("Do you trust the files in this folder?"), None);
        assert_eq!(trust_screen_keys("✻ Welcome to Claude Code!\n❯ "), None);
        assert_eq!(trust_screen_keys(""), None);
    }

    /// Opt-in LIVE run of the real session (see `live`): 9 real claude
    /// panes with the production personas, permissions and bridge, driven
    /// headless. Spends tokens. Inputs: the `NEURON_LIVE_*` variables (see
    /// [`live::LiveSpec::from_env`]). Prints the message timeline and every
    /// denied tool call, then stops the session.
    ///
    ///   cargo test --lib swarm_term::session::tests::live -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns 9 real claude REPLs and spends tokens — opt in"]
    async fn live_swarm_run() {
        let report = live::run_live(&live::LiveSpec::from_env()).await;
        assert!(report.broken.is_empty(), "{:#?}", report.broken);
    }

    /// Opt-in LIVE stress test of the shared configuration: 9 real claude
    /// REPLs boot at the SAME moment (no stagger) in `NEURON_LIVE_PROJECT`,
    /// their folder-trust screens (if any) are answered at once, and they
    /// are killed — `NEURON_LIVE_CYCLES` times (default 3). After every
    /// cycle the user's `~/.claude.json` must still be valid JSON. No
    /// prompt is ever sent, so it spends no tokens.
    ///
    ///   cargo test --lib swarm_term::session::tests::live_simultaneous -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "spawns 9 real claude REPLs on the user's config — opt in"]
    async fn live_simultaneous_boots_keep_the_shared_config_valid() {
        let project =
            PathBuf::from(std::env::var("NEURON_LIVE_PROJECT").expect("set NEURON_LIVE_PROJECT"));
        let cycles: usize = std::env::var("NEURON_LIVE_CYCLES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        let home = pane_config::real_user_home().expect("home");
        let config = |name: &str| {
            for candidate in [std::env::var_os("USERPROFILE").map(PathBuf::from), Some(home.clone())]
                .into_iter()
                .flatten()
            {
                let file = candidate.join(name);
                if file.is_file() {
                    return file;
                }
            }
            panic!("no {name}");
        };
        let claude_json = config(".claude.json");
        let valid = |file: &Path| {
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(file).unwrap())
                .map(|_| ())
        };
        valid(&claude_json).expect("~/.claude.json valid before the test");

        let (app, pool, _db) =
            crate::test_support::mock_app_with_pool_and_terminal_registry().await;
        let registry = app.state::<TerminalRegistry>().inner().clone();
        let spawn = resolve_claude_spawn().expect("claude CLI");
        let projects_dirs = pane_config::claude_projects_dirs();
        let persona_dir = tempfile::tempdir().expect("tempdir");
        for cycle in 1..=cycles {
            let mut panes: Vec<(String, String)> = Vec::new();
            let mut ids: Vec<String> = Vec::new();
            // Back to back, no stagger: all nine boot within a fraction
            // of a second.
            for agent in AGENT_IDS {
                let persona = persona_dir.path().join(format!("{agent}.md"));
                std::fs::write(&persona, "You are a stress-test pane. Do nothing.").unwrap();
                let id = pane_config::new_claude_session_id();
                ids.push(id.clone());
                let mut args = permissions::claude_permission_args(
                    SwarmTermPermissionMode::Restricted,
                    agent,
                    "swarm-term-STRESS",
                    &project_facts::ProjectFacts::default(),
                );
                args.extend(pane_config::claude_args(&id, &persona));
                let mut cmd = format!("\"{}\"", spawn.program.display());
                for a in &spawn.prefix_args {
                    cmd.push_str(&format!(" \"{a}\""));
                }
                let input = PaneSpawnInput {
                    cwd: project.display().to_string(),
                    cmd: Some(format!("{cmd} {}", permissions::join_quoted(&args))),
                    cols: Some(400),
                    rows: Some(30),
                    agent_kind: Some("claude-code".into()),
                    role: Some(agent.to_string()),
                    workspace: Some("swarm-term-stress".into()),
                    extra_env: Some(pane_config::pane_env()),
                };
                let pane = registry
                    .clone()
                    .spawn_pane(input, app.handle().clone(), pool.clone())
                    .await
                    .expect("spawn");
                panes.push((agent.to_string(), pane.id));
            }
            // Boot, answering any trust screen in all panes as it shows.
            let boot = Instant::now();
            let mut answered: HashMap<String, Instant> = HashMap::new();
            while boot.elapsed() < Duration::from_secs(45) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                for (agent, pane) in &panes {
                    let screen = pane_screen_tail(&registry, &pool, pane).await.unwrap_or_default();
                    let due = answered.get(pane).map_or(true, |t| t.elapsed() >= TRUST_ANSWER_EVERY);
                    if let (Some(keys), true) = (trust_screen_keys(&screen), due) {
                        press_trust_keys(&registry, agent, pane, keys).await;
                        answered.insert(pane.clone(), Instant::now());
                    }
                }
            }
            let answered = answered.len();
            for (_, pane) in &panes {
                let _ = registry.kill_pane(pane, &pool).await;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            let check = valid(&claude_json);
            println!(
                "cycle {cycle}: {} panes, {answered} trust answers, ~/.claude.json {}",
                panes.len(),
                if check.is_ok() { "valid" } else { "INVALID" }
            );
            for _ in 0..10 {
                if pane_config::remove_transcripts(&projects_dirs, ids.iter().map(String::as_str)).is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            check.expect("~/.claude.json stays valid JSON");
        }
    }
}

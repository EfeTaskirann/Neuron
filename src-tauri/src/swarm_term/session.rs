//! `TerminalSwarmSession` lifecycle.
//!
//! `start()` spawns 9 PTY panes (one per agent in `hierarchy::AGENT_IDS`)
//! through `TerminalRegistry::spawn_pane`, each running interactive
//! `claude` with the user-selected project as `cwd` — in a folder the
//! user has trusted ([`trust`]) and on the user's own Claude
//! configuration ([`pane_config`]). Before the spawn loop it prepares
//! `<project>/.bridgespace/<session>/{outbox,processed,rejected}/<agent>/`
//! and passes per-pane environment variables (`NEURON_BRIDGE`,
//! `NEURON_AGENT_ID`, `NEURON_OUTBOX`) so each claude REPL knows where to
//! drop outbound messages. Each pane's persona + bridge protocol is its
//! appended system prompt (`--append-system-prompt-file`); a short
//! kickoff turn is then pasted and the pane counts as ready once claude
//! answers `@<id> hazır.` in its transcript (named by the pane's
//! `--session-id`). The [`bridge::watcher::watcher_loop`] picks up the
//! JSON envelopes those drops create and delivers their body into the
//! appropriate target pane via bracketed paste.
//!
//! `stop()` kills every pane, uninstalls the bridge watcher, archives
//! `rejected/`, and deletes the `.bridgespace/<session>/` tree, the
//! panes' transcripts and the session's Neuron files.

use std::collections::{BTreeMap, HashMap, HashSet};
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
use crate::swarm_term::lifecycle::LifecycleStore;
use crate::swarm_term::pane_config::{self, PaneManifest};
use crate::swarm_term::permissions::{self, SwarmTermPermissionMode};
use crate::swarm_term::project_guard;
use crate::swarm_term::persona::{
    build_kickoff_message, build_persona_prompt, is_ready_answer,
};
use crate::swarm_term::preflight;
use crate::swarm_term::trust;

const READY_DELAY_MS: u64 = 1500;

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

        // The session's own files and every pane's claude session id —
        // on disk before any pane runs, so even after a crash the next
        // start knows which transcripts to delete.
        let session_dir = pane_config::session_dir(&app_data, &session_id);
        let claude_sessions: BTreeMap<String, String> = AGENT_IDS
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
        for (idx, &agent_id) in AGENT_IDS.iter().enumerate() {
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
                    );
                    #[cfg(test)]
                    let prompt = tests::with_injection(agent_id, prompt);
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
            let mut args =
                permissions::claude_permission_args(permission_mode, agent_id, &session_id);
            args.extend(pane_config::claude_args(&claude_sessions[agent_id], &persona_file));
            let agent_cmd = format!("{cmd} {}", permissions::join_quoted(&args));
            let mut extra_env = pane_config::pane_env();
            // Bridge paths — informational (the persona prompt carries
            // them), exported so `echo $NEURON_BRIDGE` in a pane shows
            // them while debugging.
            let outbox_self = bridgespace_root
                .join("outbox")
                .join(agent_id)
                .display()
                .to_string();
            extra_env.insert(
                "NEURON_BRIDGE".to_string(),
                bridgespace_root.display().to_string(),
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
            session_dir: Some(session_dir.clone()),
            projects_dirs: projects_dirs.clone(),
            claude_version: preflight.version.map(|v| v.to_string()),
            preflight_note: preflight.note.clone(),
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
                &claude_sessions,
                &projects_dirs,
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
        let (pane_ids, bridge, session_dir, projects_dirs, bridgespace_root): (
            Vec<String>,
            Option<BridgeHandle>,
            Option<PathBuf>,
            Vec<PathBuf>,
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
        Ok(())
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
/// * claude's folder-trust screen is answered for the user, who trusted
///   the folder in Neuron before the session could start
///   ([`trust_screen_keys`]; at most [`TRUST_ANSWER_ATTEMPTS`] times).
/// * A sent kickoff without an answer is re-sent once after
///   [`KICKOFF_RESEND_AFTER`] (it may have landed before the REPL
///   finished booting) and given up (warn, pane stays not-ready)
///   [`READY_TIMEOUT`] after it was sent.
///
/// Returns as soon as every pane is settled or the session is stopped
/// (`alive` cleared).
#[allow(clippy::too_many_arguments)]
async fn wait_for_ready_answers(
    registry: &TerminalRegistry,
    pool: &DbPool,
    panes_by_agent: &HashMap<String, String>,
    claude_sessions: &BTreeMap<String, String>,
    projects_dirs: &[PathBuf],
    ready: &Arc<Mutex<HashSet<String>>>,
    alive: &AtomicBool,
    mut kicked: HashMap<String, Option<Instant>>,
) {
    let started = Instant::now();
    let mut pending: HashSet<&str> =
        panes_by_agent.keys().map(String::as_str).collect();
    let mut resent: HashSet<String> = HashSet::new();
    let mut settle: HashMap<String, ScreenSettle> = HashMap::new();
    let mut trust_answers: HashMap<String, (u32, Instant)> = HashMap::new();
    let mut last_screen_check = Instant::now() - BLOCKED_SCREEN_POLL;
    loop {
        if !alive.load(Ordering::SeqCst) {
            return;
        }
        let answered: Vec<&str> = pending
            .iter()
            .copied()
            .filter(|agent| {
                claude_sessions.get(*agent).is_some_and(|claude_session| {
                    transcript_has_ready_answer(projects_dirs, claude_session, agent)
                })
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
                        if let Some(keys) = trust_screen_keys(&screen) {
                            let (tries, last) = trust_answers
                                .get(agent)
                                .copied()
                                .unwrap_or((0, started - TRUST_ANSWER_EVERY));
                            if tries < TRUST_ANSWER_ATTEMPTS && last.elapsed() >= TRUST_ANSWER_EVERY {
                                press_trust_keys(registry, agent, pane, keys).await;
                                trust_answers.insert(agent.to_string(), (tries + 1, Instant::now()));
                            }
                            continue;
                        }
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

/// True when the pane's claude transcript — the one named by its
/// `--session-id` — holds an assistant turn that is `agent_id`'s
/// readiness answer.
fn transcript_has_ready_answer(projects_dirs: &[PathBuf], claude_session: &str, agent_id: &str) -> bool {
    pane_config::find_transcript(projects_dirs, claude_session)
        .and_then(|file| std::fs::read_to_string(file).ok())
        .is_some_and(|text| text.lines().any(|l| line_is_ready_answer(l, agent_id)))
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
        claude_version: s.claude_version.clone(),
        preflight_note: s.preflight_note.clone(),
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
    fn the_ready_answer_is_read_from_the_panes_own_transcript_only() {
        let base = tempfile::tempdir().expect("tempdir");
        let projects = base.path().join("projects");
        let dirs = vec![projects.clone()];
        assert!(!transcript_has_ready_answer(&dirs, "mine", "scout"));
        let dir = projects.join("C--proj");
        std::fs::create_dir_all(&dir).unwrap();
        // Another session in the same project — e.g. the user's own —
        // saying the phrase does not make this pane ready.
        std::fs::write(dir.join("theirs.jsonl"), format!("{READY_LINE}\n")).unwrap();
        let mine = dir.join("mine.jsonl");
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n")).unwrap();
        assert!(!transcript_has_ready_answer(&dirs, "mine", "scout"));
        std::fs::write(&mine, format!("{KICKOFF_ECHO}\n{READY_LINE}\n")).unwrap();
        assert!(transcript_has_ready_answer(&dirs, "mine", "scout"));
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
        // The operator picked the folder: the user's "I trust it" in the UI.
        trust::trust(&_pool, &project).await.expect("trust the project");
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
        let (panes, ready, root, claude_sessions) = {
            let g = swarm.inner.lock().unwrap();
            let s = g.as_ref().expect("session");
            (
                s.panes_by_agent.clone(),
                Arc::clone(&s.ready_panes),
                s.bridgespace_root.clone().unwrap(),
                PaneManifest::read(s.session_dir.as_deref().unwrap())
                    .expect("pane manifest")
                    .claude_sessions,
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

        // Readiness (the session answers claude's folder-trust screen
        // itself). `NEURON_LIVE_SCREEN_AT=<n>`: print the orchestrator's
        // screen at n s.
        let screen_at = std::env::var("NEURON_LIVE_SCREEN_AT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut screen_shown = false;
        let ready_deadline = Instant::now() + Duration::from_secs(150);
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

        // Denied tool calls + transcript copies, before stop() deletes the
        // transcripts.
        let projects_dirs = pane_config::claude_projects_dirs();
        std::fs::create_dir_all(&out).ok();
        for agent in AGENT_IDS {
            let Some(file) = pane_config::find_transcript(&projects_dirs, &claude_sessions[*agent])
            else {
                println!("NO TRANSCRIPT [{agent}]");
                continue;
            };
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
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

        swarm.stop(app.handle().clone()).await.expect("stop");
        println!("[{}] session stopped", stamp(Instant::now()));
        let left: Vec<&str> = claude_sessions
            .iter()
            .filter(|(_, id)| pane_config::find_transcript(&projects_dirs, id).is_some())
            .map(|(agent, _)| agent.as_str())
            .collect();
        assert!(left.is_empty(), "transcripts left behind: {left:?}");
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

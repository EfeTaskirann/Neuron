//! `TerminalSwarmSession` lifecycle.
//!
//! `start()` spawns 9 PTY panes (one per agent in `hierarchy::AGENT_IDS`)
//! through `TerminalRegistry::spawn_pane`, each running interactive
//! `claude` with the user-selected project as `cwd`. The resulting
//! pane_ids are kept in `panes_by_agent` so Phase 3 (persona injection)
//! and Phase 4 (router) can look them up by agent_id.
//!
//! `stop()` kills every pane in the active session via
//! `TerminalRegistry::kill_pane` and clears the slot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, EventId, Manager, Runtime};
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::PaneSpawnInput;
use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm::binding::resolve_claude_spawn;
use crate::swarm::profile::ProfileRegistry;
use crate::swarm_term::hierarchy::{allowed_for, AGENT_IDS};

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
/// `Configuration Error: invalid JSON` (the user hit this in the
/// 2026-05-12 22:11Z smoke; all 9 panes died with `exit 1`).
/// 500 ms is generous against observed first-write timing and
/// imperceptible against the 11–13 s persona-injection budget.
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
    pub router_listeners: Vec<EventId>,
    /// Per-session HOME isolation root (under
    /// `app_data_dir/swarm-term/homes/<session_id>`). Cleaned up
    /// in `stop()`. `None` for synthetic / test sessions that
    /// bypass the spawn loop.
    pub homes_root: Option<PathBuf>,
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
        // Build the spawn command. On Windows the resolver may have
        // swapped the .cmd wrapper for `node.exe <cli.js>` to bypass
        // the PTY-incompatible batch detach trick — in that case
        // `prefix_args` carries the cli.js path.
        //
        // `--add-dir` is intentionally omitted: portable-pty already
        // sets `cwd` on the child via `builder.cwd(&cwd)`, so claude
        // reads the project root naturally without the extra flag
        // (which on some versions makes the REPL exit silently).
        //
        // Permission mode: we use `--permission-mode bypassPermissions`
        // (the documented mode enum value) instead of the
        // `--dangerously-skip-permissions` toggle. Functionally the
        // two share the "no per-tool approval prompts" outcome, but
        // the explicit `--dangerously-…` flag advertises itself as
        // sandbox-only and claude responds by re-running its safety
        // confirmation (the dialog the user reported as "asks for
        // auth again on every pane"). The mode value is the
        // first-class way to set the same effect without tripping
        // that gate, so we ship 9 panes that just open.
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

        // Per-session HOME isolation root. Each pane gets a private
        // subdir with copies of `~/.claude.json` + `~/.claude/.credentials.json`
        // so the 9 claude.exe processes don't race on the user's
        // real `~/.claude.json` and truncate it (the 2026-05-12 23:46Z
        // smoke saw 27000 → 1023 bytes corruption from concurrent writes).
        // Cleaned up in `stop()` via `fs::remove_dir_all`.
        let session_id = format!("swarm-term-{}", Ulid::new());
        let homes_root = prepare_isolated_homes_root(&app, &session_id)?;

        let mut panes_by_agent: HashMap<String, String> = HashMap::new();
        let mut spawned: Vec<String> = Vec::new();
        for (idx, &agent_id) in AGENT_IDS.iter().enumerate() {
            // Stagger spawns so each claude.exe finishes its
            // `~/.claude.json` startup write before the next process
            // opens the same file in write mode. With per-pane HOME
            // isolation the race window is eliminated structurally,
            // but stagger stays as defence-in-depth + UI smoothing
            // (panes appear one at a time instead of in a burst).
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
                    // Roll back already-spawned panes + bail.
                    for pid in &spawned {
                        let _ = registry.kill_pane(pid, &pool).await;
                    }
                    let _ = std::fs::remove_dir_all(&homes_root);
                    return Err(AppError::Internal(format!(
                        "seed_pane_home({agent_id}): {e}"
                    )));
                }
            };
            let mut extra_env: HashMap<String, String> = HashMap::new();
            let pane_home_str = pane_home.display().to_string();
            extra_env.insert("HOME".to_string(), pane_home_str.clone());
            extra_env.insert("USERPROFILE".to_string(), pane_home_str);
            let input = PaneSpawnInput {
                cwd: project_str.clone(),
                cmd: Some(cmd.clone()),
                // PTY width matters for marker integrity, not for
                // agent reasoning capacity. claude renders long
                // assistant text wrapped at the terminal width, and
                // `router::strip_ansi` then sees the wrap as a `\n`
                // — the marker body gets split across two PTY lines
                // and only the first segment routes to the target,
                // so receivers report "mesaj yarım geldi". 400 cols
                // keeps virtually every realistic marker body on a
                // single line. xterm.js renders its own visual wrap
                // independently, so the user's 3×3 grid view is
                // unaffected.
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
                    // Roll back any panes we already spawned.
                    for pid in &spawned {
                        let _ = registry.kill_pane(pid, &pool).await;
                    }
                    return Err(AppError::Internal(format!(
                        "spawn agent `{agent_id}`: {e}"
                    )));
                }
            }
        }

        // Install the routing service BEFORE inserting the session
        // — once listeners are live, any marker line in a pane fires
        // a route. Persona injection (below) is what kicks the chain
        // off, so listeners need to be ready first.
        let router_listeners =
            crate::swarm_term::router::install(app.clone(), panes_by_agent.clone());

        let session = ActiveSession {
            session_id,
            project_dir: project_dir.clone(),
            panes_by_agent: panes_by_agent.clone(),
            router_listeners,
            homes_root: Some(homes_root),
        };
        let handle = handle_from(&session);
        {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            *guard = Some(session);
        }

        // Fire-and-forget persona injection: wait for the claude REPLs
        // to settle, then paste each persona body + routing-protocol
        // section into its pane. Errors are logged but don't abort the
        // session — a missing persona just means that pane stays in
        // its default claude REPL state.
        let app_for_inject = app.clone();
        let registry_for_inject = registry.clone();
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
                let body = build_persona_payload(agent_id, &profile.body);
                if let Err(e) = registry_for_inject
                    .write_to_pane(pane_id, body.as_bytes())
                    .await
                {
                    tracing::warn!(
                        agent_id = %agent_id,
                        error = %e,
                        "swarm-term: persona injection write failed"
                    );
                }
            }

            // Auto-prompt hook for reproducible smoke tests. If the
            // `NEURON_TERM_AUTO_PROMPT` env var is set, wait for the
            // orchestrator to finish rendering its `@orchestrator
            // hazır.` ack (claude takes 5–10 s to settle after a
            // bracketed paste), then paste the prompt into the
            // orchestrator pane as if the user had typed it. Makes
            // end-to-end swarm runs scriptable from the CLI:
            //
            //   $env:NEURON_TERM_AUTO_PROMPT='deep dive to neuron project and improve it'
            //   pnpm tauri dev
            //
            // Empty / unset env = no auto-prompt (the default
            // production behaviour: orchestrator waits for the
            // user's first message in its xterm pane).
            if let Ok(auto_prompt) = std::env::var("NEURON_TERM_AUTO_PROMPT") {
                let trimmed = auto_prompt.trim();
                if !trimmed.is_empty() {
                    tokio::time::sleep(Duration::from_millis(AUTO_PROMPT_DELAY_MS)).await;
                    if let Some(orch_pane) = panes_by_agent.get("orchestrator") {
                        // Wrap in xterm bracketed paste so claude
                        // treats it as a single user submission
                        // instead of N Enter-split lines.
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

            // Optional second pane keeps the AppHandle alive for the
            // duration of the injection — without this Rust drops it
            // immediately and the writes still complete (the registry
            // doesn't need the handle), but holding it documents
            // intent.
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
        let (pane_ids, listeners, homes_root): (
            Vec<String>,
            Vec<EventId>,
            Option<PathBuf>,
        ) = {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            match guard.take() {
                Some(s) => (
                    s.panes_by_agent.into_values().collect(),
                    s.router_listeners,
                    s.homes_root,
                ),
                None => return Ok(()),
            }
        };
        // Unlisten before killing panes so the router doesn't fire on
        // shutdown noise (orphan claude prints before SIGTERM lands).
        crate::swarm_term::router::uninstall(&app, listeners);
        let registry = app.state::<TerminalRegistry>().inner().clone();
        let pool = app.state::<DbPool>().inner().clone();
        for pid in pane_ids {
            let _ = registry.kill_pane(&pid, &pool).await;
        }
        // Clean up the isolated HOME directory tree after all panes
        // are dead. claude.exe stops writing to its per-pane
        // `.claude.json` once the process exits, so `remove_dir_all`
        // is safe to run unconditionally — no race against an active
        // writer. Errors here are logged but don't fail the stop
        // call (the temp homes accumulate harmlessly until the next
        // session if removal fails for any reason).
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
        Ok(())
    }
}

/// Create the per-session HOME isolation root under
/// `app_data_dir/swarm-term/homes/<session_id>/`. The directory is
/// fresh each session — no carry-over between sessions, no cleanup
/// race against running panes.
fn prepare_isolated_homes_root<R: Runtime>(
    app: &AppHandle<R>,
    session_id: &str,
) -> Result<PathBuf, AppError> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Internal(format!("app_data_dir: {e}")))?;
    let root = app_data
        .join("swarm-term")
        .join("homes")
        .join(session_id);
    std::fs::create_dir_all(&root).map_err(|e| {
        AppError::Internal(format!("mkdir {}: {e}", root.display()))
    })?;
    Ok(root)
}

/// Seed a per-pane HOME directory by copying the user's real
/// `~/.claude.json` + `~/.claude/.credentials.json` into the pane's
/// isolated home. Once the claude.exe in that pane has its own
/// private `~/.claude.json` to read/write, the 9-way concurrent-write
/// race on the real shared file is eliminated.
///
/// The copies are one-shot snapshots taken at session-start. claude's
/// runtime writes (tipsHistory, lastPlanModeUse, etc.) go to the
/// per-pane copies and are discarded when the session ends. This
/// keeps the user's real config stable across sessions; the only
/// downside is per-pane state diverges over time within a long
/// session (unlikely to matter — these counters are cosmetic).
fn seed_pane_home(
    homes_root: &Path,
    agent_id: &str,
) -> Result<PathBuf, AppError> {
    let pane_home = homes_root.join(agent_id);
    std::fs::create_dir_all(&pane_home).map_err(|e| {
        AppError::Internal(format!("mkdir {}: {e}", pane_home.display()))
    })?;
    let real_home = real_user_home()?;
    let real_claude_json = real_home.join(".claude.json");
    let real_claude_dir = real_home.join(".claude");

    // Top-level: copy `~/.claude.json` (one file, ~27 KB).
    if real_claude_json.is_file() {
        std::fs::copy(&real_claude_json, pane_home.join(".claude.json"))
            .map_err(|e| {
                AppError::Internal(format!(
                    "copy {} → pane: {e}",
                    real_claude_json.display()
                ))
            })?;
    }

    // `~/.claude/` directory: shallow copy of FILES (skip
    // subdirectories). The 2026-05-13 00:41Z smoke proved that
    // copying only `.credentials.json` is not enough — claude.exe
    // exited within ~15 s when its expected `settings.json` and
    // friends were missing. The full file set in the user's
    // `~/.claude/` includes:
    //   .credentials.json        (OAuth tokens, ~500 B)
    //   .last-cleanup            (timestamp, ~24 B)
    //   history.jsonl            (prompt history, can be 100s of KB)
    //   mcp-needs-auth-cache.json
    //   settings.json            (claude settings, ~200 B)
    //   settings.local.json
    // …plus subdirs (backups/, cache/, file-history/, paste-cache/,
    // plans/, plugins/, projects/, session-env/, sessions/,
    // shell-snapshots/, tasks/).
    //
    // We copy every TOP-LEVEL FILE, including history.jsonl (the
    // most expensive at ~100 KB), so claude has the complete
    // settings surface. Subdirectories are not copied — claude
    // recreates whatever it needs on demand and the runtime state
    // there (sessions, plans, projects) is naturally per-process
    // anyway. This keeps the disk footprint to ~9 × ~150 KB ≈
    // 1.4 MB per swarm-term session, cleaned up in `stop()`.
    let pane_claude_dir = pane_home.join(".claude");
    std::fs::create_dir_all(&pane_claude_dir).map_err(|e| {
        AppError::Internal(format!(
            "mkdir {}: {e}",
            pane_claude_dir.display()
        ))
    })?;
    if real_claude_dir.is_dir() {
        if let Ok(read) = std::fs::read_dir(&real_claude_dir) {
            for entry in read.flatten() {
                let src = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let Some(name) = src.file_name() else { continue };
                let dst = pane_claude_dir.join(name);
                if let Err(e) = std::fs::copy(&src, &dst) {
                    tracing::warn!(
                        agent_id = %agent_id,
                        src = %src.display(),
                        error = %e,
                        "swarm-term: pane home seed — file copy failed (non-fatal)"
                    );
                }
            }
        }
    }
    Ok(pane_home)
}

/// Resolve the user's real home directory. Mirrors the same
/// fallback chain that `swarm::binding::home_dir` uses (HOME first,
/// USERPROFILE on Windows) since we don't want to take a new
/// `dirs` crate dependency just for one call site.
fn real_user_home() -> Result<PathBuf, AppError> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    if cfg!(target_os = "windows") {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            if !profile.is_empty() {
                return Ok(PathBuf::from(profile));
            }
        }
    }
    Err(AppError::Internal(
        "cannot resolve user home directory (HOME and USERPROFILE both unset)".into(),
    ))
}

/// Build the persona-injection payload for one agent.
///
/// The body wraps the persona text + the routing-protocol footer in
/// terminal **bracketed-paste** escape sequences (`\x1b[200~ … \x1b[201~`)
/// so claude's REPL treats it as a single pasted message instead of
/// nine separate Enter-submitted lines, then submits with a final `\r`.
///
/// Visible in the user's xterm pane verbatim — that's the point. The
/// user sees the persona render, claude reads it, and the first
/// assistant response acknowledges the role.
fn build_persona_payload(agent_id: &str, body: &str) -> String {
    let allowed: Vec<String> =
        allowed_for(agent_id).iter().map(|s| s.to_string()).collect();
    // NOTE: the example marker lines below use HTML entities
    // (`&gt;&gt;`) instead of literal `>>` so that when claude's
    // REPL renders this persona body back to its PTY at injection
    // time, the example lines do NOT fire phantom routes. The
    // marker parser correctly rejects the `&gt;&gt;` prefix (pinned
    // by `substring_fallback_rejects_html_escaped_marker_in_docs`).
    //
    // The PROSE that EXPLAINS the syntax to claude refers to the
    // marker characters by NAME (`iki adet "greater-than" işareti`,
    // also written `>>` plainly inside paragraph prose). Earlier
    // attempt at v3 had `\`&gt;\` yerine literal \`&gt;\`` — both
    // halves were the same escaped entity, which read as a
    // tautology and confused claude into emitting `&gt;&gt;` in its
    // real dispatches (78-second silence in the 2026-05-12 23:45Z
    // smoke). This v4 keeps prose mentions of `>>` literal because
    // they're inline (not at column-0 line start, so the marker
    // regex won't false-positive on them).
    let routing = format!(
        "\n\n## Routing protocol — KRİTİK\n\n\
         Sen bu swarm'ın bir ajanısın (`{agent_id}`). Diğer ajanlara\n\
         mesaj yollamak için **satır başında**, dekoratörsüz, kendi\n\
         başına bir satır olarak şunu yaz:\n\n\
             [iki tane > karakteri] [boşluk] @<hedef-ajan-id> [iki nokta] [boşluk] <mesaj gövdesi>\n\n\
         Yani literal olarak: chevron-chevron + space + @ + agent-id + colon + space + body.\n\
         Aşağıda örnekler verirken bu chevron çiftini `&gt;&gt;` olarak\n\
         HTML-escape ediyorum (persona injection sırasında yanlışlıkla\n\
         route fire etmesinler diye); SEN gerçek dispatch yazarken\n\
         doğrudan iki greater-than karakterini (ASCII 0x3E ikilisi) kullan.\n\n\
         **Syntax örnekleri (sen `&gt;&gt;` yerine düz iki chevron yaz):**\n\n\
         &gt;&gt; @scout: src-tauri/src/foo.rs dosyasındaki `bar` fonksiyonunu bul\n\
         &gt;&gt; @planner: scout sonucuna göre 3 maddelik plan çıkar\n\n\
         **Yanlış örnekler (router yakalamaz):**\n\n\
         - `Şimdi scout'a soracağım: api/auth.ts'i oku` (chevron yok)\n\
         - `Sıradaki: @scout: api'yi oku` (chevron yok)\n\
         - `\"...\" şeklinde @scout: ...` (chevron yok)\n\n\
         **Kurallar:**\n\n\
         1. Routing satırını başka metinle aynı satıra koyma. Tek başına bir satır.\n\
         2. Birden fazla ajana gönderecekseniz her birini ayrı satıra yaz.\n\
         3. Senin izin verilen destinasyonların: {allowed_list}.\n\
         4. İzin verilmeyen hedefe yazarsan sistem RoutingOverlay'de `denied`\n\
            etiketiyle gösterir; başka hedef seç ya da coordinator'a sor.\n\
         5. Sana gelen mesajların altında `— from @<gönderen>` imzası olur —\n\
            kime cevap vereceğini bu imzaya bakarak belirle.\n\
         6. Sana gelen mesajı CEVAP'INDA verbatim alıntılama (`>>` satırını\n\
            kopyalama). Paraphrase et. Yoksa router senin echo'nu yeni\n\
            dispatch sanıp yine fire eder.\n\n\
         ## Çalışma protokolü (4-state contract) — KRİTİK\n\n\
         **Eğer dispatch ALAN tarafsan** (örn. scout'a `>> @scout: X` geldi):\n\
         Sessiz kalma. 4 durumdan birine gir ve gönderene mutlaka bildir:\n\n\
         1. **alındı** (5 saniye içinde): `&gt;&gt; @<gönderen>: alındı — <bir cümlelik anlayışın>`.\n\
            Acknowledgement; sender bekleyecek mi yoksa kayıp mı bilir.\n\
         2. **tamam** (iş bittiğinde): `&gt;&gt; @<gönderen>: tamam — <sonuç özeti,\n\
            dosya yolları, ne değişti>`. Bu completion signal'i; sender\n\
            bir sonraki adıma geçer.\n\
         3. **belirsiz** (dispatch net değilse): `&gt;&gt; @<gönderen>: belirsiz —\n\
            <spesifik sorun: hangi dosya? hangi tür değişiklik?>` ve DUR.\n\
            KESİNLİKLE tahmin yapma; tahminle çalışırsan reviewer reject eder.\n\
         4. **hata** (yapamadıysan): `&gt;&gt; @<gönderen>: hata — <somut sebep:\n\
            dosya yok / compile fail / tool izin yok>` ve dur.\n\n\
         **Eğer dispatch GÖNDEREN tarafsan** (örn. orchestrator/coordinator):\n\
         Alıcıdan dönen state markerına BAK ve buna göre davran:\n\n\
         - `alındı —` aldıysan: SUS. Specialist çalışıyor; ikinci dispatch atma,\n\
           polling yapma. `tamam`'ı bekle.\n\
         - `tamam —` aldıysan: completion'ı kabul et, bir sonraki faza geç\n\
           (Faz 1 → 2 → 3 sırası, ya da Faz 3 paralel dispatchlerinden bir\n\
           sonrakine).\n\
         - `belirsiz —` aldıysan: aynı vague task'i tekrar gönderme; specialist'in\n\
           sorduğu spesifik sorunu (dosya/değişiklik tipi/kabul kriteri) çöz ve\n\
           yeni dispatch yaz.\n\
         - `hata —` aldıysan: retry mı, alternative specialist mi, escalate mi\n\
           karar ver. Aynı dispatch'i tekrar yollama (3 kez denersen reviewer\n\
           bunu rejected bir verdict sayar).\n\n\
         Bu 4 state swarm'ın state machine'i. Çift yönlü kontrat — receiver\n\
         state üretir, sender state tüketir. Eksiksiz uy.\n\n\
         **İLK YANIT (kullanıcı/route gelmeden önce):** Bu persona mesajını\n\
         aldıktan sonra **yalnızca** şunu yaz ve sus: `@{agent_id} hazır.`\n\
         — başka tek karakter yazma. Bir sonraki kullanıcı/route mesajını\n\
         bekle.",
        agent_id = agent_id,
        allowed_list = if allowed.is_empty() {
            "(yok)".to_string()
        } else {
            allowed.join(", ")
        },
    );

    // Bracketed-paste start, body + routing footer, bracketed-paste end,
    // then `\r` to submit. Trailing `\r` is intentional: it mimics the
    // Enter keystroke xterm's `onData` sends.
    format!("\x1b[200~{body}{routing}\x1b[201~\r")
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

// Used by Phase 4 router for pane_id ↔ agent_id lookups.
#[allow(dead_code)]
pub(crate) fn lookup_session<'a>(
    inner: &'a Option<ActiveSession>,
) -> Option<&'a ActiveSession> {
    inner.as_ref()
}

#[allow(dead_code)]
pub(crate) fn project_path_for_session(_s: &ActiveSession) -> &Path {
    _s.project_dir.as_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_is_bracketed_paste_wrapped() {
        let p = build_persona_payload("scout", "Hello body");
        assert!(p.starts_with('\x1b'));
        assert!(p.contains("\x1b[200~"));
        assert!(p.contains("\x1b[201~"));
        assert!(p.ends_with('\r'));
    }

    #[test]
    fn payload_carries_persona_body_verbatim() {
        let body = "# Scout\n\nFind things.\n";
        let p = build_persona_payload("scout", body);
        assert!(p.contains(body));
    }

    #[test]
    fn payload_includes_routing_protocol_and_allowed_destinations() {
        let p = build_persona_payload("scout", "x");
        assert!(p.contains("## Routing protocol"));
        // v4 footer describes the marker shape in bracketed prose
        // (so the description itself can't fire phantom routes) and
        // shows examples in HTML-escaped form. Both signatures must
        // be present.
        assert!(p.contains("[iki tane > karakteri]"));
        assert!(p.contains("&gt;&gt; @scout:"));
        assert!(p.contains("coordinator"));
        assert!(p.contains("orchestrator"));
    }

    #[test]
    fn payload_example_lines_do_not_contain_literal_marker_at_col0() {
        // Belt-and-suspenders: literal `>> @<real-agent>:` at the
        // start of a line would still fire a route at injection
        // time, because the marker regex's relaxed prefix lets it
        // match. So the persona footer MUST escape every `>>` in
        // examples; this test pins that property by asserting no
        // line in the payload starts with `>> @<known-agent>:`.
        let p = build_persona_payload("orchestrator", "x");
        for line in p.split('\n') {
            assert!(
                !line.starts_with(">> @"),
                "persona footer has bare `>> @` at column 0 — example would fire a phantom route: {line}"
            );
        }
    }

    #[test]
    fn payload_includes_lifecycle_protocol() {
        // The 4-state contract (alındı / tamam / belirsiz / hata) is
        // load-bearing for stopping `ne yapayım?` heartbeat loops.
        let p = build_persona_payload("scout", "x");
        assert!(p.contains("alındı"));
        assert!(p.contains("tamam"));
        assert!(p.contains("belirsiz"));
        assert!(p.contains("hata"));
    }

    #[test]
    fn payload_for_isolated_agent_says_yok() {
        // Hypothetical: an agent with no allowed destinations would
        // render `(yok)`. Our hardcoded graph has none, but the helper
        // is still defensible.
        let allowed = allowed_for("nobody");
        assert!(allowed.is_empty());
        let p = build_persona_payload("nobody", "x");
        assert!(p.contains("(yok)"));
    }
}

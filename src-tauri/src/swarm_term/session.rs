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

        let mut panes_by_agent: HashMap<String, String> = HashMap::new();
        let mut spawned: Vec<String> = Vec::new();
        for &agent_id in AGENT_IDS {
            let input = PaneSpawnInput {
                cwd: project_str.clone(),
                cmd: Some(cmd.clone()),
                cols: Some(120),
                rows: Some(30),
                agent_kind: Some("claude-code".into()),
                role: Some(agent_id.to_string()),
                workspace: Some("swarm-term".into()),
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
            session_id: format!("swarm-term-{}", Ulid::new()),
            project_dir: project_dir.clone(),
            panes_by_agent: panes_by_agent.clone(),
            router_listeners,
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
        let (pane_ids, listeners): (Vec<String>, Vec<EventId>) = {
            let mut guard = self.inner.lock().map_err(|_| {
                AppError::Internal("swarm-term registry poisoned".into())
            })?;
            match guard.take() {
                Some(s) => (
                    s.panes_by_agent.into_values().collect(),
                    s.router_listeners,
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
        Ok(())
    }
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
    let routing = format!(
        "\n\n## Routing protocol — KRİTİK\n\n\
         Sen bu swarm'ın bir ajanısın (`{agent_id}`). Diğer ajanlara\n\
         mesaj yönlendirmenin TEK YOLU şu literal formatı **kendi başına\n\
         bir satırda**, satır başında, dekoratörsüz yazmak:\n\n\
         >> @<agent-id>: <mesaj>\n\n\
         **Doğru örnekler (bu satırlar router tarafından yakalanır):**\n\n\
         >> @scout: api/auth.ts dosyasını oku ve özet çıkar\n\
         >> @planner: scout sonucunu 3 maddelik plana çevir\n\n\
         **Yanlış örnekler (router YAKALAYAMAZ, route düşmez):**\n\n\
         - `Şimdi scout'a soracağım: api/auth.ts'i oku` (markerless prose)\n\
         - `- >> @scout: ...` (yine olur ama bullet ekleme gereksiz)\n\
         - `Sıradaki: @scout: api'yi oku` (`>>` yok)\n\
         - `\"...\" şeklinde @scout: ...` (`>>` yok)\n\n\
         **Kurallar:**\n\n\
         1. Routing satırını başka metinle aynı satıra koyma.\n\
         2. Birden fazla ajana gönderecekseniz her birini ayrı satıra\n\
            yaz — sırayla ikisi de route edilir.\n\
         3. Senin izin verilen destinasyonların: {allowed_list}.\n\
         4. İzin verilmeyen hedefe yazarsan sistem RoutingOverlay'de\n\
            `denied` etiketiyle gösterir; başka hedef seç.\n\
         5. Sana gelen mesajların altında `— from @<gönderen>` imzası\n\
            olur — kime cevap verdiğini bu imzaya bakarak belirle.\n\n\
         **İLK YANIT:** Bu mesajı aldıktan sonra **yalnızca** şunu yaz\n\
         ve sus: `@{agent_id} hazır.` — başka tek karakter yazma. Bir\n\
         sonraki kullanıcı / route mesajını bekle.",
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
        assert!(p.contains(">> @<agent-id>: <mesaj>"));
        assert!(p.contains("coordinator"));
        assert!(p.contains("orchestrator"));
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

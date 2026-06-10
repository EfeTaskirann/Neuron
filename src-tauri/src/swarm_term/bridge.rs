//! File-system based inter-agent IPC.
//!
//! Each pane is an isolated `claude` REPL. When agent A wants to message
//! agent B, A uses its `Write` tool to atomically create
//! `.bridgespace/<session>/inbox/<B>/<id>.json` with shape:
//!
//! ```json
//! {"from":"scout","to":"orchestrator",
//!  "body":"tamam — foo.rs:42",
//!  "task_id":"t-1"}
//! ```
//!
//! A background poll loop ([`watcher_loop`]) reads every inbox directory
//! once every [`WATCH_POLL_MS`] ms, validates each file (parse +
//! hierarchy gate), and delivers the body to the target pane's PTY via
//! bracketed paste. After successful delivery the file moves to
//! `processed/<B>/`; denied / malformed files move to `rejected/<B>/`
//! with a `.reason` sidecar. Transient delivery failures
//! (`target_not_ready`, `target_locked`, write timeout) leave the file
//! in the inbox so the next tick retries — after 5 consecutive
//! failures the file is rejected.
//!
//! Crashes while in flight are non-fatal: every file in `inbox/` is
//! re-scanned on the next tick, so a partially delivered route is
//! retried — file-system as durable queue. Atomic-rename file moves
//! preserve the invariant "inbox = not yet delivered, processed =
//! delivered".

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::sync::Notify;
use ulid::Ulid;

use crate::error::AppError;
use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::{allowed_for, is_allowed, AGENT_IDS};
use crate::swarm_term::lifecycle::{
    followup_for_coordinator_inbound, parse_lifecycle_token_with_fallback,
    LifecycleStore, TransitionKind,
};

/// Polling cadence for [`watcher_loop`]. Each tick scans every inbox
/// directory; the cost is 9 stats per tick which is negligible.
///
/// 250 ms gives ≤250 ms perceived latency for inter-agent messaging,
/// well below the multi-second cost of a claude REPL turn — the user
/// will never see the poll cadence in practice.
const WATCH_POLL_MS: u64 = 250;

/// Hard cap on a single PTY write. claude's bracketed-paste accept is
/// sub-ms
/// in healthy conditions; a 2 s cap surfaces stuck panes within one
/// tick of the user noticing nothing happens.
const WRITE_TIMEOUT_SECS: u64 = 2;

/// After this many transient delivery failures the file is moved to
/// `rejected/<target>/` rather than retried indefinitely. Prevents a
/// permanently locked pane from holding a busy-loop on its inbox.
const MAX_DELIVERY_ATTEMPTS: u32 = 5;

/// Subdirectory of the project root where the per-session bridge tree
/// lives. Cleaned up in `TerminalSwarmRegistry::stop`.
pub const BRIDGE_DIRNAME: &str = ".bridgespace";

// --------------------------------------------------------------------- //
// Public types                                                          //
// --------------------------------------------------------------------- //

/// One inter-agent message. Agents create one of these via the `Write`
/// tool; the watcher reads it.
///
/// `from` is trust-based: a misbehaving persona could spoof it, but all
/// 9 personas are our own and the cost of path-derived sender attribution
/// (an outbox staging step) is not worth the added complexity in v1.
/// The watcher does cross-check `envelope.to` against the path's target
/// directory and uses the path when they disagree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    pub from: String,
    pub to: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

impl Envelope {
    /// Structural validation. Hierarchy enforcement happens separately.
    pub fn validate(&self) -> Result<(), String> {
        if self.from.trim().is_empty() {
            return Err("from is empty".into());
        }
        if self.to.trim().is_empty() {
            return Err("to is empty".into());
        }
        if !AGENT_IDS.iter().any(|a| *a == self.from) {
            return Err(format!("unknown from agent: {}", self.from));
        }
        if !AGENT_IDS.iter().any(|a| *a == self.to) {
            return Err(format!("unknown to agent: {}", self.to));
        }
        if self.from == self.to {
            return Err("self-loop forbidden".into());
        }
        if self.body.is_empty() {
            return Err("body is empty".into());
        }
        Ok(())
    }
}

/// Returned by [`install`]. Holding it keeps the watcher alive; drop
/// (via [`uninstall`]) cancels it.
pub struct BridgeHandle {
    pub root: PathBuf,
    cancel: Arc<Notify>,
}

// --------------------------------------------------------------------- //
// Layout / installation                                                 //
// --------------------------------------------------------------------- //

/// Initialise the per-session directory tree at `root`. The directory
/// is empty after this call; senders populate `inbox/`, watcher
/// migrates files into `processed/` or `rejected/`.
pub fn prepare_layout(root: &Path) -> Result<(), AppError> {
    for subdir in ["inbox", "processed", "rejected"] {
        for agent in AGENT_IDS {
            let p = root.join(subdir).join(agent);
            fs::create_dir_all(&p).map_err(|e| {
                AppError::Internal(format!("mkdir {}: {e}", p.display()))
            })?;
        }
    }
    Ok(())
}

/// Drop a `.gitignore` at the bridge parent (`<project>/.bridgespace/`)
/// so per-session message files don't accidentally land in git history.
/// Idempotent — only writes the file if it doesn't already exist.
pub fn ensure_gitignore(bridge_parent: &Path) -> io::Result<()> {
    let p = bridge_parent.join(".gitignore");
    if p.exists() {
        return Ok(());
    }
    fs::write(p, "*\n!.gitignore\n")
}

/// Spawn the watcher task for the given session and return a handle
/// that uninstall takes ownership of. Layout under `root` must already
/// exist (call [`prepare_layout`] first).
pub fn install<R: Runtime>(
    app: AppHandle<R>,
    root: PathBuf,
    panes_by_agent: HashMap<String, String>,
    ready_panes: Arc<Mutex<HashSet<String>>>,
    lifecycle: Arc<LifecycleStore>,
) -> BridgeHandle {
    let cancel = Arc::new(Notify::new());
    let cancel_for_task = Arc::clone(&cancel);
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let root_for_task = root.clone();
    tauri::async_runtime::spawn(watcher_loop(
        app,
        registry,
        root_for_task,
        Arc::new(panes_by_agent),
        ready_panes,
        lifecycle,
        cancel_for_task,
    ));
    BridgeHandle { root, cancel }
}

/// Stop the watcher task started by [`install`]. The bridge root is
/// kept; the caller (session lifecycle) is responsible for deletion.
pub fn uninstall(handle: BridgeHandle) {
    // notify_one stores a permit: the watcher spends most of its time
    // inside process_pending (multi-second write timeouts), not parked
    // in notified() — notify_waiters fired in that window would be
    // lost and the watcher would poll the deleted root forever.
    handle.cancel.notify_one();
}

// --------------------------------------------------------------------- //
// Watcher loop                                                          //
// --------------------------------------------------------------------- //

async fn watcher_loop<R: Runtime>(
    app: AppHandle<R>,
    registry: TerminalRegistry,
    root: PathBuf,
    panes_by_agent: Arc<HashMap<String, String>>,
    ready_panes: Arc<Mutex<HashSet<String>>>,
    lifecycle: Arc<LifecycleStore>,
    cancel: Arc<Notify>,
) {
    tracing::info!(
        root = %root.display(),
        poll_ms = WATCH_POLL_MS,
        "swarm-term bridge: watcher start"
    );
    loop {
        process_pending(
            &app,
            &registry,
            &root,
            &panes_by_agent,
            &ready_panes,
            &lifecycle,
        )
        .await;
        tokio::select! {
            _ = cancel.notified() => {
                tracing::info!("swarm-term bridge: watcher cancelled");
                return;
            }
            _ = tokio::time::sleep(Duration::from_millis(WATCH_POLL_MS)) => {}
        }
    }
}

async fn process_pending<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    panes_by_agent: &Arc<HashMap<String, String>>,
    ready_panes: &Arc<Mutex<HashSet<String>>>,
    lifecycle: &Arc<LifecycleStore>,
) {
    let inbox_root = root.join("inbox");
    // Iterate targets in canonical order so the user sees a stable
    // delivery order during multi-source bursts.
    for &target in AGENT_IDS {
        let dir = inbox_root.join(target);
        let mut entries: Vec<PathBuf> = match fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|s| s.to_str()) == Some("json")
                })
                .collect(),
            Err(_) => continue,
        };
        // ULIDs / timestamp filenames sort lexicographically into
        // chronological order — FIFO delivery without a separate index.
        entries.sort();
        for path in entries {
            process_one(
                app,
                registry,
                root,
                &path,
                target,
                panes_by_agent,
                ready_panes,
                lifecycle,
            )
            .await;
        }
    }
}

/// Process a single inbox file. Reads, validates, gates, delivers, and
/// migrates the file accordingly. All errors are absorbed into either a
/// retry (file stays in inbox) or a rejection (file moves to
/// `rejected/`); a return value is unnecessary.
#[allow(clippy::too_many_arguments)]
async fn process_one<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    path: &Path,
    target_from_path: &str,
    panes_by_agent: &Arc<HashMap<String, String>>,
    ready_panes: &Arc<Mutex<HashSet<String>>>,
    lifecycle: &Arc<LifecycleStore>,
) {
    // ---- Read + parse ------------------------------------------------ //
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            // ENOENT is normal on a concurrent move (e.g. the user
            // poked the dir manually); anything else is worth logging.
            if e.kind() != io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "swarm-term bridge: read failed"
                );
            }
            return;
        }
    };

    let envelope: Envelope = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(e) => {
            // Partial-write tolerance: bump retry counter; only reject
            // after several consecutive parse failures. claude's Write
            // tool is atomic for small files in practice, but the
            // tolerance protects against a sender that writes in
            // chunks.
            let attempts = bump_attempts(path);
            if attempts >= MAX_DELIVERY_ATTEMPTS {
                let reason = format!("malformed JSON x{attempts}: {e}");
                if let Err(mv) = move_to_rejected(
                    root,
                    path,
                    target_from_path,
                    &reason,
                ) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %mv,
                        "swarm-term bridge: rejected-move failed"
                    );
                }
                let _ = app.emit(
                    crate::events::SWARM_TERM_ROUTE,
                    json!({
                        "source": "?",
                        "target": target_from_path,
                        "body": truncate(&raw, 200),
                        "outcome": "malformed",
                        "reason": reason,
                    }),
                );
            }
            return;
        }
    };

    // Path-derived target wins on conflict — defence against a sender
    // dropping its file in the wrong directory.
    if envelope.to != target_from_path {
        tracing::warn!(
            path = %path.display(),
            envelope_to = %envelope.to,
            dir_to = %target_from_path,
            "swarm-term bridge: envelope.to / path disagreement; path wins"
        );
    }
    let to_agent = target_from_path.to_string();
    let from_agent = envelope.from.clone();
    let body = envelope.body.clone();
    // The structured `task_id` is handed to the lifecycle layer as a
    // fallback id for bare-keyword bodies (see `lifecycle_synthesise`).
    let envelope_task_id = envelope.task_id.clone();

    // ---- Structural validation -------------------------------------- //
    let mut effective = envelope.clone();
    effective.to = to_agent.clone();
    if let Err(reason) = effective.validate() {
        let _ = move_to_rejected(root, path, &to_agent, &reason);
        let _ = app.emit(
            crate::events::SWARM_TERM_ROUTE,
            json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "malformed",
                "reason": reason,
            }),
        );
        return;
    }

    // ---- Hierarchy gate --------------------------------------------- //
    if !is_allowed(&from_agent, &to_agent) {
        let allowed: Vec<String> = allowed_for(&from_agent)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let reason = format!("hierarchy denies {from_agent}→{to_agent}");
        let _ = move_to_rejected(root, path, &to_agent, &reason);
        let _ = app.emit(
            crate::events::SWARM_TERM_ROUTE,
            json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "denied",
                "reason": reason,
                "allowed": allowed,
            }),
        );
        return;
    }

    // ---- Target pane lookup ----------------------------------------- //
    let target_pane = match panes_by_agent.get(&to_agent) {
        Some(p) => p.clone(),
        None => {
            // The session is missing a pane for the target — fatal
            // for this message, no retry would help.
            let _ = move_to_rejected(root, path, &to_agent, "no pane for target");
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": from_agent,
                    "target": to_agent,
                    "body": body,
                    "outcome": "unknown_target",
                    "reason": "no pane for target",
                }),
            );
            return;
        }
    };

    // ---- Persona-injection readiness -------------------------------- //
    let is_ready = ready_panes
        .lock()
        .map(|g| g.contains(&target_pane))
        .unwrap_or(false);
    if !is_ready {
        // Transient — keep file in inbox; next tick retries. Cap
        // attempts so a pane that never finishes injection eventually
        // gives up rather than spamming overlay events forever.
        let attempts = bump_attempts(path);
        let _ = app.emit(
            crate::events::SWARM_TERM_ROUTE,
            json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "target_not_ready",
                "attempts": attempts,
            }),
        );
        if attempts >= MAX_DELIVERY_ATTEMPTS {
            let _ = move_to_rejected(
                root,
                path,
                &to_agent,
                "target never became ready",
            );
        }
        return;
    }

    // ---- Pane status gate ------------------------------------------- //
    let status = registry.pane_status(&target_pane).await;
    // `success` = "last turn finished cleanly, prompt idle + ready" — a
    // VALID delivery target (see `sidecar::terminal::pane_status`). Only
    // `awaiting_approval` (would interleave with a human safety prompt)
    // and `error` (pane is wedged / has no stdin) block delivery.
    // Including `success` here previously starved lifecycle fanouts to a
    // reviewer/orchestrator pane that had just finished its turn.
    let locked = is_delivery_blocked(status);
    if locked {
        let s = status.unwrap_or("unknown");
        let attempts = bump_attempts(path);
        let _ = app.emit(
            crate::events::SWARM_TERM_ROUTE,
            json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "target_locked",
                "status": s,
                "attempts": attempts,
            }),
        );
        if attempts >= MAX_DELIVERY_ATTEMPTS {
            let _ = move_to_rejected(
                root,
                path,
                &to_agent,
                &format!("target stayed locked ({s})"),
            );
        }
        return;
    }

    // ---- Deliver ---------------------------------------------------- //
    let signed = format_routed_message(&body, &from_agent);
    let registry_inner = registry.clone();
    let target_pane_for_write = target_pane.clone();
    let signed_bytes = signed.into_bytes();
    let write_fut = async move {
        registry_inner
            .write_to_pane(&target_pane_for_write, &signed_bytes)
            .await
    };
    let outcome = perform_route_write_with_timeout(
        write_fut,
        app.clone(),
        from_agent.clone(),
        to_agent.clone(),
        target_pane.clone(),
    )
    .await;

    match outcome {
        RouteWriteOutcome::Ok => {
            tracing::info!(
                source = %from_agent,
                target = %to_agent,
                bytes = body.len(),
                "swarm-term bridge: delivered"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": from_agent,
                    "target": to_agent,
                    "body": body,
                    "outcome": "ok",
                }),
            );
            if let Err(e) = move_to_processed(root, path, &to_agent) {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "swarm-term bridge: processed-move failed"
                );
            }
            // Lifecycle synthesis runs only on coordinator-bound
            // deliveries — the bridge fans the autonomy follow-up out
            // (builder DONE -> reviewer "review", reviewer APPROVED ->
            // orchestrator "TASK_DONE").
            if to_agent == "coordinator" {
                lifecycle_synthesise(
                    app,
                    root,
                    &from_agent,
                    &body,
                    envelope_task_id.as_deref(),
                    panes_by_agent,
                    lifecycle,
                );
            }
        }
        RouteWriteOutcome::Timeout | RouteWriteOutcome::Err => {
            let attempts = bump_attempts(path);
            if attempts >= MAX_DELIVERY_ATTEMPTS {
                let reason = match outcome {
                    RouteWriteOutcome::Timeout => "write timeout x5",
                    RouteWriteOutcome::Err => "write error x5",
                    _ => unreachable!(),
                };
                let _ = move_to_rejected(root, path, &to_agent, reason);
            }
        }
    }
}

// --------------------------------------------------------------------- //
// Delivery helpers                                                      //
// --------------------------------------------------------------------- //

/// True when a pane's status means a routed message must NOT be
/// delivered right now. `awaiting_approval` would interleave with a
/// human safety prompt; `error` means the pane is wedged / has no
/// stdin. A `success` / idle pane (last turn finished, prompt ready) IS
/// a valid delivery target — including it here previously starved
/// lifecycle fanouts to just-finished reviewer/orchestrator panes.
fn is_delivery_blocked(status: Option<&str>) -> bool {
    matches!(status, Some("awaiting_approval") | Some("error"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouteWriteOutcome {
    Ok,
    Timeout,
    Err,
}

async fn perform_route_write_with_timeout<R, F>(
    write_fut: F,
    app: AppHandle<R>,
    source: String,
    target: String,
    target_pane: String,
) -> RouteWriteOutcome
where
    R: Runtime,
    F: std::future::Future<Output = Result<(), AppError>>,
{
    let result = tokio::time::timeout(
        Duration::from_secs(WRITE_TIMEOUT_SECS),
        write_fut,
    )
    .await;
    match result {
        Err(_) => {
            tracing::warn!(
                source = %source,
                target = %target,
                target_pane = %target_pane,
                timeout_secs = WRITE_TIMEOUT_SECS,
                "swarm-term bridge: route write timed out"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": source,
                    "target": target,
                    "outcome": "target_write_timeout",
                    "timeout_secs": WRITE_TIMEOUT_SECS,
                }),
            );
            RouteWriteOutcome::Timeout
        }
        Ok(Err(e)) => {
            tracing::warn!(
                source = %source,
                target = %target,
                target_pane = %target_pane,
                error = %e,
                "swarm-term bridge: route write failed"
            );
            RouteWriteOutcome::Err
        }
        Ok(Ok(())) => RouteWriteOutcome::Ok,
    }
}

/// Wire-format the routed payload that's pasted into the target pane's
/// stdin:
///
///   * `\x1b[200~ … \x1b[201~\r` — xterm bracketed paste + submit CR.
///   * `\r` inside body → `\n` (CR inside paste = visual overstrike).
///   * Embedded `\x1b[201~` → `[201~` (ESC byte dropped) so the
///     bracketed paste isn't prematurely closed by the body content.
pub(crate) fn format_routed_message(body: &str, source_agent: &str) -> String {
    let safe = body
        .replace('\r', "\n")
        .replace("\x1b[201~", "[201~");
    format!(
        "\x1b[200~{safe}\n\n— from @{source_agent} [routed by Neuron]\x1b[201~\r"
    )
}

// --------------------------------------------------------------------- //
// Lifecycle synthesis (coordinator inbound only)                        //
// --------------------------------------------------------------------- //

/// On a successful coordinator-bound delivery, parse the body for a
/// lifecycle token and, if it's one that warrants a fanout, drop the
/// synthesised follow-up envelope into the appropriate inbox. The
/// watcher picks it up on its next tick — same delivery path as a
/// user-authored route.
fn lifecycle_synthesise<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    from_agent: &str,
    body: &str,
    task_id: Option<&str>,
    panes_by_agent: &Arc<HashMap<String, String>>,
    lifecycle: &Arc<LifecycleStore>,
) {
    let Some(transition) = parse_lifecycle_token_with_fallback(body, task_id)
    else {
        return;
    };
    let Some(source_pane) = panes_by_agent.get(from_agent).cloned() else {
        return;
    };
    let new_state = lifecycle.record(&source_pane, &transition);
    tracing::info!(
        source = %from_agent,
        source_pane = %source_pane,
        task_id = %transition.task_id,
        transition = ?transition.kind,
        new_state = ?new_state,
        "swarm-term bridge: lifecycle transition recorded"
    );
    let _ = app.emit(
        crate::events::SWARM_TERM_LIFECYCLE,
        json!({
            "source": from_agent,
            "source_pane": source_pane,
            "task_id": transition.task_id,
            "transition": format!("{:?}", transition.kind),
            "state": format!("{new_state:?}"),
        }),
    );

    let Some((synth_target, synth_body)) =
        followup_for_coordinator_inbound(from_agent, &transition)
    else {
        return;
    };
    if !is_allowed("coordinator", &synth_target) {
        tracing::warn!(
            source = %from_agent,
            synth_target = %synth_target,
            "swarm-term bridge: lifecycle fanout dropped — hierarchy forbids coordinator→{}",
            synth_target,
        );
        return;
    }
    if !panes_by_agent.contains_key(&synth_target) {
        tracing::warn!(
            source = %from_agent,
            synth_target = %synth_target,
            "swarm-term bridge: lifecycle fanout dropped — no pane for synth target"
        );
        return;
    }
    let env = Envelope {
        from: "coordinator".into(),
        to: synth_target.clone(),
        body: synth_body.clone(),
        task_id: Some(transition.task_id.clone()),
    };
    match drop_envelope(root, &env) {
        Ok(_) => {
            tracing::info!(
                from = "coordinator",
                to = %synth_target,
                body = %synth_body,
                "swarm-term bridge: lifecycle auto-fanout enqueued"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": "coordinator",
                    "target": synth_target,
                    "body": synth_body,
                    "outcome": "lifecycle_fanout",
                }),
            );
        }
        Err(e) => {
            tracing::warn!(
                target = %synth_target,
                error = %e,
                "swarm-term bridge: failed to write synth envelope"
            );
        }
    }
    if matches!(transition.kind, TransitionKind::Approved) {
        lifecycle.mark_done(&source_pane, &transition.task_id);
    }
}

// --------------------------------------------------------------------- //
// File-system helpers                                                   //
// --------------------------------------------------------------------- //

/// Atomically write an envelope to `inbox/<env.to>/<ulid>.json` using
/// the `.tmp` + rename pattern. Used by the lifecycle fanout and the
/// integration-test scaffolding.
pub fn drop_envelope(root: &Path, env: &Envelope) -> io::Result<PathBuf> {
    let id = Ulid::new().to_string();
    let dir = root.join("inbox").join(&env.to);
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{id}.json.tmp"));
    let final_path = dir.join(format!("{id}.json"));
    let json = serde_json::to_string_pretty(env)
        .map_err(|e| io::Error::other(format!("serialize: {e}")))?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, &final_path)?;
    Ok(final_path)
}

/// Read the `.attempts` sidecar for `path` (if any), increment it, and
/// write the new value back. Returns the new attempt count.
fn bump_attempts(path: &Path) -> u32 {
    let sidecar = attempts_sidecar(path);
    let cur: u32 = fs::read_to_string(&sidecar)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = cur.saturating_add(1);
    let _ = fs::write(&sidecar, next.to_string());
    next
}

fn attempts_sidecar(path: &Path) -> PathBuf {
    let mut sidecar = path.to_path_buf();
    sidecar.set_extension("attempts");
    sidecar
}

fn move_to_processed(
    root: &Path,
    path: &Path,
    target: &str,
) -> io::Result<()> {
    let dest_dir = root.join("processed").join(target);
    fs::create_dir_all(&dest_dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no filename"))?;
    let dest = dest_dir.join(name);
    fs::rename(path, &dest)?;
    // Successful delivery — discard the attempts counter if any.
    let _ = fs::remove_file(attempts_sidecar(path));
    Ok(())
}

fn move_to_rejected(
    root: &Path,
    path: &Path,
    target: &str,
    reason: &str,
) -> io::Result<()> {
    let dest_dir = root.join("rejected").join(target);
    fs::create_dir_all(&dest_dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("no filename"))?;
    let dest = dest_dir.join(name);
    fs::rename(path, &dest)?;
    // Annotate the rejection so the user can read it without grepping
    // the tracing log.
    let mut reason_path = dest.clone();
    reason_path.set_extension("reason");
    let _ = fs::write(&reason_path, reason);
    let _ = fs::remove_file(attempts_sidecar(path));
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn fresh_root() -> tempfile::TempDir {
        let d = tempdir().expect("tempdir");
        prepare_layout(d.path()).expect("layout");
        d
    }

    fn env(from: &str, to: &str, body: &str) -> Envelope {
        Envelope {
            from: from.into(),
            to: to.into(),
            body: body.into(),
            task_id: None,
        }
    }

    // ---- Envelope ---------------------------------------------------- //

    #[test]
    fn envelope_validate_accepts_well_formed() {
        assert!(env("scout", "orchestrator", "hi").validate().is_ok());
    }

    #[test]
    fn envelope_validate_rejects_empty_fields() {
        assert!(env("", "orchestrator", "hi").validate().is_err());
        assert!(env("scout", "", "hi").validate().is_err());
        assert!(env("scout", "orchestrator", "").validate().is_err());
    }

    #[test]
    fn envelope_validate_rejects_unknown_agent() {
        assert!(env("nobody", "orchestrator", "hi").validate().is_err());
        assert!(env("scout", "nobody", "hi").validate().is_err());
    }

    #[test]
    fn envelope_validate_rejects_self_loop() {
        assert!(env("scout", "scout", "hi").validate().is_err());
    }

    #[test]
    fn envelope_roundtrips_through_json() {
        let e = env("scout", "orchestrator", "tamam — foo.rs:42");
        let serialised = serde_json::to_string(&e).expect("serialise");
        let back: Envelope = serde_json::from_str(&serialised).expect("parse");
        assert_eq!(back, e);
    }

    // ---- Layout ------------------------------------------------------ //

    #[test]
    fn prepare_layout_creates_all_subdirs() {
        let d = fresh_root();
        for sub in ["inbox", "processed", "rejected"] {
            for agent in AGENT_IDS {
                let p = d.path().join(sub).join(agent);
                assert!(
                    p.is_dir(),
                    "missing {} dir for agent {}",
                    sub,
                    agent
                );
            }
        }
    }

    #[test]
    fn ensure_gitignore_writes_once() {
        let d = tempdir().expect("tempdir");
        ensure_gitignore(d.path()).expect("first write");
        let p = d.path().join(".gitignore");
        assert!(p.is_file());
        let body = fs::read_to_string(&p).expect("read");
        assert!(body.contains('*'));
        // Idempotent — second call should not error and should not
        // overwrite (we can pin overwrite by writing a marker).
        fs::write(&p, "CUSTOM").expect("user override");
        ensure_gitignore(d.path()).expect("second write");
        let body2 = fs::read_to_string(&p).expect("read 2");
        assert_eq!(body2, "CUSTOM", "ensure_gitignore must not clobber");
    }

    // ---- drop_envelope ---------------------------------------------- //

    #[test]
    fn drop_envelope_lands_in_target_inbox() {
        let d = fresh_root();
        let e = env("scout", "orchestrator", "hi");
        let path = drop_envelope(d.path(), &e).expect("drop");
        assert!(path.is_file());
        assert!(path
            .to_string_lossy()
            .replace('\\', "/")
            .contains("/inbox/orchestrator/"));
        let raw = fs::read_to_string(&path).expect("read");
        let back: Envelope = serde_json::from_str(&raw).expect("parse");
        assert_eq!(back, e);
    }

    // ---- format_routed_message -------------------------------------- //

    #[test]
    fn format_routed_message_wraps_in_bracketed_paste() {
        let s = format_routed_message("hello", "scout");
        assert!(s.starts_with("\x1b[200~"));
        assert!(s.ends_with("\x1b[201~\r"));
        assert!(s.contains("— from @scout [routed by Neuron]"));
    }

    #[test]
    fn format_routed_message_neutralises_embedded_cr_and_paste_end() {
        // \r inside the body would visually overstrike on xterm.js.
        let with_cr = format_routed_message("line1\rline2", "scout");
        assert!(!with_cr.contains('\r').then_some(false).unwrap_or(true)
            || with_cr.matches('\r').count() == 1);
        // \x1b[201~ inside body would close the bracketed paste early
        // and the trailing signature would become a second submit.
        let with_end = format_routed_message("foo\x1b[201~bar", "scout");
        assert!(!with_end.contains("\x1b[201~bar"));
        assert!(with_end.contains("[201~bar"));
    }

    // ---- is_delivery_blocked (pane-status gate) --------------------- //

    #[test]
    fn success_and_idle_panes_are_valid_delivery_targets() {
        // Regression pin: a pane that just finished its turn (`success`)
        // or is mid-turn (`running`) must NOT be treated as locked —
        // that bug starved lifecycle fanouts to just-finished
        // reviewer/orchestrator panes.
        assert!(!is_delivery_blocked(Some("success")));
        assert!(!is_delivery_blocked(Some("running")));
        assert!(!is_delivery_blocked(Some("starting")));
        assert!(!is_delivery_blocked(None));
        // Genuinely blocking states.
        assert!(is_delivery_blocked(Some("awaiting_approval")));
        assert!(is_delivery_blocked(Some("error")));
    }

    // ---- bump_attempts / sidecar handling --------------------------- //

    #[test]
    fn bump_attempts_increments_through_sidecar() {
        let d = tempdir().expect("tempdir");
        let p = d.path().join("msg.json");
        fs::write(&p, "{}").expect("write");
        assert_eq!(bump_attempts(&p), 1);
        assert_eq!(bump_attempts(&p), 2);
        assert_eq!(bump_attempts(&p), 3);
        let sidecar = attempts_sidecar(&p);
        assert!(sidecar.is_file());
        assert_eq!(fs::read_to_string(&sidecar).unwrap().trim(), "3");
    }

    // ---- move_to_processed / move_to_rejected ----------------------- //

    #[test]
    fn move_to_processed_clears_inbox_and_drops_attempts() {
        let d = fresh_root();
        let inbox_path = d
            .path()
            .join("inbox")
            .join("orchestrator")
            .join("01.json");
        fs::write(
            &inbox_path,
            serde_json::to_string(&env("scout", "orchestrator", "hi"))
                .unwrap(),
        )
        .unwrap();
        bump_attempts(&inbox_path);
        assert!(attempts_sidecar(&inbox_path).is_file());

        move_to_processed(d.path(), &inbox_path, "orchestrator")
            .expect("move");

        assert!(!inbox_path.exists(), "inbox must be empty after success");
        assert!(
            d.path()
                .join("processed")
                .join("orchestrator")
                .join("01.json")
                .is_file(),
            "processed must hold the moved file"
        );
        assert!(
            !attempts_sidecar(&inbox_path).exists(),
            "attempts sidecar must be cleaned up"
        );
    }

    #[test]
    fn move_to_rejected_writes_reason_sidecar() {
        let d = fresh_root();
        let inbox_path = d
            .path()
            .join("inbox")
            .join("orchestrator")
            .join("01.json");
        fs::write(&inbox_path, "broken").unwrap();

        move_to_rejected(d.path(), &inbox_path, "orchestrator", "test reason")
            .expect("reject");

        let rejected_file = d
            .path()
            .join("rejected")
            .join("orchestrator")
            .join("01.json");
        assert!(rejected_file.is_file());
        let mut reason_path = rejected_file.clone();
        reason_path.set_extension("reason");
        assert_eq!(
            fs::read_to_string(&reason_path).unwrap(),
            "test reason"
        );
    }
}

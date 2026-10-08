//! The background poll loop and the per-file delivery state machine.
//!
//! [`watcher_loop`] scans every inbox directory once per tick;
//! [`process_one`] runs a single envelope through read → parse →
//! validate → hierarchy gate → pane lookup → readiness/status gate →
//! timed write, migrating the file into `processed/` or `rejected/`
//! (or leaving it for retry) as it goes. Every permanent rejection is
//! also reported to the SENDING agent (see [`super::notify`]).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::Notify;

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::{allowed_for, is_allowed, AGENT_IDS};
use crate::swarm_term::lifecycle::LifecycleStore;

use super::delivery::{
    format_routed_message, is_delivery_blocked, paste_then_submit,
    perform_route_write_with_timeout, truncate, RouteWriteOutcome,
};
use super::envelope::{sniff_from, Envelope};
use super::fsops::{bump_attempts, move_to_processed, move_to_rejected};
use super::notify::{notify_agent, rejection_notice};
use super::stall::{check_stalls, STALL_CHECK_EVERY};
use super::synth::lifecycle_synthesise;

/// Polling cadence for [`watcher_loop`]. Each tick scans every inbox
/// directory; the cost is 9 stats per tick which is negligible.
///
/// 250 ms gives ≤250 ms perceived latency for inter-agent messaging,
/// well below the multi-second cost of a claude REPL turn — the user
/// will never see the poll cadence in practice.
pub(super) const WATCH_POLL_MS: u64 = 250;

/// Attempt cap for failures that waiting does not fix: an unparseable
/// file (the cap only absorbs a sender still mid-write) and PTY write
/// errors / timeouts.
const MAX_DELIVERY_ATTEMPTS: u32 = 5;

/// How long a message may wait for its target pane to finish persona
/// injection. Time-based, not attempt-based: 5 × 250 ms (~1 s) used to
/// discard messages sent to a pane that was still booting.
pub(super) const NOT_READY_GRACE: Duration = Duration::from_secs(60);

/// How long a message may wait on a target pane in a blocking status
/// (see [`is_delivery_blocked`]) before it is rejected.
pub(super) const LOCKED_GRACE: Duration = Duration::from_secs(180);

/// While a message waits, re-emit its routing event only on the first
/// attempt and every Nth after it (~5 s at the poll cadence), instead
/// of flooding the Routing Log with one row per 250 ms tick.
const RETRY_EVENT_EVERY: u32 = 20;

pub(super) async fn watcher_loop<R: Runtime>(
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
    let mut last_stall_check = Instant::now();
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
        if last_stall_check.elapsed() >= STALL_CHECK_EVERY {
            check_stalls(&app, &registry, &panes_by_agent, &lifecycle).await;
            last_stall_check = Instant::now();
        }
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
        // FIFO by write time (mtime; renames preserve it), name as the
        // tie-break. Sorting by name alone assumed chronological ids,
        // but agents pick their own filenames (and were seen copying
        // the persona's example id verbatim).
        entries.sort_by_cached_key(|p| {
            let mtime = fs::metadata(p).and_then(|m| m.modified()).ok();
            (mtime, p.clone())
        });
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

/// How long `path` has been waiting in the inbox: the file's age (the
/// sender wrote it; renames preserve mtime), floored by the attempt
/// count × poll cadence in case the filesystem reports no mtime.
fn waited(path: &Path, attempts: u32) -> Duration {
    let by_attempts =
        Duration::from_millis(WATCH_POLL_MS.saturating_mul(attempts as u64));
    let by_mtime = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .unwrap_or_default();
    by_attempts.max(by_mtime)
}

fn should_emit_retry_event(attempts: u32) -> bool {
    attempts == 1 || attempts % RETRY_EVENT_EVERY == 0
}

/// Move `path` to `rejected/` and, when that succeeds and the sender is
/// a known agent, paste a `[Neuron]` notice into the sender's pane. The
/// notice is gated on the move so a file that could not be moved (and
/// will be re-processed next tick) does not notify twice.
#[allow(clippy::too_many_arguments)]
async fn reject_and_notify<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    path: &Path,
    panes_by_agent: &HashMap<String, String>,
    sender: Option<&str>,
    target: &str,
    reason: &str,
    body: &str,
) {
    if let Err(e) = move_to_rejected(root, path, target, reason) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "swarm-term bridge: rejected-move failed"
        );
        return;
    }
    let Some(sender) = sender.filter(|s| AGENT_IDS.contains(s)) else {
        return;
    };
    let text = rejection_notice(sender, target, reason, body);
    notify_agent(registry, panes_by_agent, sender, &text).await;
    emit_notice(app, sender, &text, reason);
}

/// Routing Log row for a `[Neuron]` notice pasted into `recipient`'s pane.
fn emit_notice<R: Runtime>(app: &AppHandle<R>, recipient: &str, text: &str, reason: &str) {
    let _ = app.emit(
        crate::events::SWARM_TERM_ROUTE,
        json!({
            "source": "neuron",
            "target": recipient,
            "body": format!("[Neuron] {text}"),
            "outcome": "notice",
            "reason": reason,
        }),
    );
}

/// Process a single inbox file. Reads, validates, gates, delivers, and
/// migrates the file accordingly. All errors are absorbed into either a
/// retry (file stays in inbox) or a rejection (file moves to
/// `rejected/` and the sender is notified); a return value is
/// unnecessary.
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

    let mut envelope = match Envelope::parse_lenient(&raw) {
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
                let sender = sniff_from(&raw);
                let _ = app.emit(
                    crate::events::SWARM_TERM_ROUTE,
                    json!({
                        "source": sender.as_deref().unwrap_or("?"),
                        "target": target_from_path,
                        "body": truncate(&raw, 200),
                        "outcome": "malformed",
                        "reason": reason,
                    }),
                );
                reject_and_notify(
                    app,
                    registry,
                    root,
                    path,
                    panes_by_agent,
                    sender.as_deref(),
                    target_from_path,
                    &reason,
                    &raw,
                )
                .await;
            }
            return;
        }
    };
    envelope.normalize();

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
        reject_and_notify(
            app,
            registry,
            root,
            path,
            panes_by_agent,
            Some(from_agent.as_str()),
            &to_agent,
            &reason,
            &body,
        )
        .await;
        return;
    }

    // ---- Hierarchy gate --------------------------------------------- //
    if !is_allowed(&from_agent, &to_agent) {
        let allowed: Vec<String> = allowed_for(&from_agent)
            .iter()
            .map(|s| s.to_string())
            .collect();
        let reason = format!("hierarchy denies {from_agent}→{to_agent}");
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
        reject_and_notify(
            app,
            registry,
            root,
            path,
            panes_by_agent,
            Some(from_agent.as_str()),
            &to_agent,
            &reason,
            &body,
        )
        .await;
        return;
    }

    // ---- Target pane lookup ----------------------------------------- //
    let target_pane = match panes_by_agent.get(&to_agent) {
        Some(p) => p.clone(),
        None => {
            // The session is missing a pane for the target — fatal
            // for this message, no retry would help.
            let reason = "no pane for target";
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": from_agent,
                    "target": to_agent,
                    "body": body,
                    "outcome": "unknown_target",
                    "reason": reason,
                }),
            );
            reject_and_notify(
                app,
                registry,
                root,
                path,
                panes_by_agent,
                Some(from_agent.as_str()),
                &to_agent,
                reason,
                &body,
            )
            .await;
            return;
        }
    };

    // ---- Persona-injection readiness -------------------------------- //
    let is_ready = ready_panes
        .lock()
        .map(|g| g.contains(&target_pane))
        .unwrap_or(false);
    if !is_ready {
        // Transient — keep file in inbox; next tick retries until the
        // message has waited NOT_READY_GRACE, then give up and tell
        // the sender.
        let attempts = bump_attempts(path);
        let expired = waited(path, attempts) >= NOT_READY_GRACE;
        if expired || should_emit_retry_event(attempts) {
            let mut ev = json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "target_not_ready",
                "attempts": attempts,
            });
            if expired {
                ev["outcome"] = json!("rejected");
                ev["reason"] = json!(format!(
                    "target never became ready ({}s)",
                    NOT_READY_GRACE.as_secs()
                ));
            }
            let _ = app.emit(crate::events::SWARM_TERM_ROUTE, ev);
        }
        if expired {
            let reason = format!(
                "target never became ready ({}s)",
                NOT_READY_GRACE.as_secs()
            );
            reject_and_notify(
                app,
                registry,
                root,
                path,
                panes_by_agent,
                Some(from_agent.as_str()),
                &to_agent,
                &reason,
                &body,
            )
            .await;
        }
        return;
    }

    // ---- Pane status gate ------------------------------------------- //
    let status = registry.pane_status(&target_pane).await;
    // `success` = "last turn finished cleanly, prompt idle + ready" — a
    // VALID delivery target (see `sidecar::terminal::pane_status`).
    // Only `error` (pane is wedged / has no stdin) blocks delivery; see
    // `is_delivery_blocked` for why `awaiting_approval` does not.
    let locked = is_delivery_blocked(status);
    if locked {
        let s = status.unwrap_or("unknown");
        let attempts = bump_attempts(path);
        let expired = waited(path, attempts) >= LOCKED_GRACE;
        if expired || should_emit_retry_event(attempts) {
            let mut ev = json!({
                "source": from_agent,
                "target": to_agent,
                "body": body,
                "outcome": "target_locked",
                "status": s,
                "attempts": attempts,
            });
            if expired {
                ev["outcome"] = json!("rejected");
                ev["reason"] = json!(format!(
                    "target stayed {s} for {}s",
                    LOCKED_GRACE.as_secs()
                ));
            }
            let _ = app.emit(crate::events::SWARM_TERM_ROUTE, ev);
        }
        if expired {
            let reason = format!(
                "target stayed locked ({s}) for {}s",
                LOCKED_GRACE.as_secs()
            );
            reject_and_notify(
                app,
                registry,
                root,
                path,
                panes_by_agent,
                Some(from_agent.as_str()),
                &to_agent,
                &reason,
                &body,
            )
            .await;
        }
        return;
    }

    // ---- Deliver ---------------------------------------------------- //
    let signed = format_routed_message(&body, &from_agent);
    let registry_inner = registry.clone();
    let target_pane_for_write = target_pane.clone();
    let signed_bytes = signed.into_bytes();
    let write_fut = async move {
        paste_then_submit(&registry_inner, &target_pane_for_write, &signed_bytes)
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
                let notice = lifecycle_synthesise(
                    app,
                    root,
                    &from_agent,
                    &body,
                    envelope_task_id.as_deref(),
                    panes_by_agent,
                    lifecycle,
                );
                if let Some(text) = notice {
                    tracing::warn!(
                        source = %from_agent,
                        "swarm-term bridge: lifecycle-shaped body not recognised"
                    );
                    notify_agent(registry, panes_by_agent, &from_agent, &text)
                        .await;
                    emit_notice(
                        app,
                        &from_agent,
                        &text,
                        "lifecycle signal not recognised; no auto-fanout",
                    );
                }
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
                let _ = app.emit(
                    crate::events::SWARM_TERM_ROUTE,
                    json!({
                        "source": from_agent,
                        "target": to_agent,
                        "body": body,
                        "outcome": "rejected",
                        "reason": reason,
                    }),
                );
                reject_and_notify(
                    app,
                    registry,
                    root,
                    path,
                    panes_by_agent,
                    Some(from_agent.as_str()),
                    &to_agent,
                    reason,
                    &body,
                )
                .await;
            }
        }
    }
}

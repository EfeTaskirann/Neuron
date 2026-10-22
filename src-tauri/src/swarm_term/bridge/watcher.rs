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
use crate::swarm_term::lifecycle::{misaddressed_signal, LifecycleStore};

use super::delivery::{
    format_routed_message, is_delivery_blocked, submit_pasted,
    perform_route_write_with_timeout, truncate, RouteWriteOutcome,
};
use super::envelope::{sniff_from, Envelope};
use super::fsops::{bump_attempts, move_to_processed, move_to_rejected};
use super::notify::{notify_agent, rejection_notice};
use super::stall::{check_stalls, stall_check_every, STALL_CADENCE_REFRESH};
use super::synth::{
    delivery_task_id, dispatch_conflict, lifecycle_synthesise, note_coordinator_report,
    note_dispatch,
};

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

/// Largest inbox file the bridge will read. Real envelopes are a few
/// KB; anything bigger is rejected unread.
pub(super) const MAX_ENVELOPE_BYTES: u64 = 256 * 1024;

/// While a message waits, re-emit its routing event only on the first
/// attempt and every Nth after it (~5 s at the poll cadence), instead
/// of flooding the Routing Log with one row per 250 ms tick.
const RETRY_EVENT_EVERY: u32 = 20;

/// Where a settled outbox file belongs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SettledDest {
    Processed { target: String },
    Rejected { bucket: String, reason: String },
}

/// An outbox file whose outcome is final — delivered, or rejected with
/// the sender told — but whose move out of the outbox failed (an
/// antivirus scan or a sync client holding the file). Processing it
/// again would paste it a second time and re-run the lifecycle
/// follow-ups, so later ticks only retry the move.
#[derive(Debug, Clone)]
struct StuckMove {
    dest: SettledDest,
    /// The file when it settled: a changed mtime or size means the
    /// sender has since written a new message under the same name.
    /// `None` while the file's metadata cannot be read (the same lock
    /// that failed the move can fail the stat); the first readable
    /// stamp is adopted.
    stamp: Option<FileStamp>,
}

/// `(mtime, size)` of a file.
type FileStamp = (Option<SystemTime>, u64);

/// Settled files still waiting for their move, keyed by outbox path.
type StuckMoves = HashMap<PathBuf, StuckMove>;

fn file_stamp(path: &Path) -> Option<FileStamp> {
    fs::metadata(path).ok().map(|m| (m.modified().ok(), m.len()))
}

/// Remember that `path` is settled for `dest` although its move failed.
/// Recorded even when its metadata cannot be read: skipping it would
/// let the next scan process (and paste) it again. A file that turns
/// out to be gone is dropped by the first retry.
fn note_stuck(stuck: &mut StuckMoves, path: &Path, dest: SettledDest) {
    stuck.insert(
        path.to_path_buf(),
        StuckMove {
            dest,
            stamp: file_stamp(path),
        },
    );
}

fn settle(root: &Path, path: &Path, dest: &SettledDest) -> io::Result<()> {
    match dest {
        SettledDest::Processed { target } => move_to_processed(root, path, target),
        SettledDest::Rejected { bucket, reason } => move_to_rejected(root, path, bucket, reason),
    }
}

/// Retry the moves that failed on earlier ticks. An entry whose file now
/// holds a different message is dropped — the scan that follows then
/// processes it as new — and so is one whose file is gone (the move
/// reports `NotFound`). Unreadable metadata alone never drops an entry.
fn retry_stuck_moves(root: &Path, stuck: &mut StuckMoves) {
    stuck.retain(|path, entry| {
        let now = match fs::metadata(path) {
            Ok(m) => Some((m.modified().ok(), m.len())),
            // Gone: nothing left to move.
            Err(e) if e.kind() == io::ErrorKind::NotFound => return false,
            // Unreadable (still locked): judge by the move below.
            Err(_) => None,
        };
        match (entry.stamp, now) {
            (Some(was), Some(is)) if was != is => return false,
            (None, Some(_)) => entry.stamp = now,
            _ => {}
        }
        match settle(root, path, &entry.dest) {
            Ok(()) => {
                tracing::info!(
                    path = %path.display(),
                    "swarm-term bridge: settled file moved on retry"
                );
                false
            }
            Err(e) => e.kind() != io::ErrorKind::NotFound,
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn watcher_loop<R: Runtime>(
    app: AppHandle<R>,
    registry: TerminalRegistry,
    root: PathBuf,
    shared_panes: super::SharedPanes,
    ready_panes: Arc<Mutex<HashSet<String>>>,
    lifecycle: Arc<LifecycleStore>,
    control: Arc<super::BridgeControl>,
    cancel: Arc<Notify>,
) {
    tracing::info!(
        root = %root.display(),
        poll_ms = WATCH_POLL_MS,
        "swarm-term bridge: watcher start"
    );
    let mut last_stall_check = Instant::now();
    let mut stall_every = stall_check_every();
    let mut stall_every_read = Instant::now();
    let mut stuck = StuckMoves::new();
    let mut was_paused = false;
    loop {
        // Paused (usage limit, budget, the user): every message waits in
        // its outbox, in order, until the bridge runs again.
        if control.is_paused() {
            was_paused = true;
            tokio::select! {
                _ = cancel.notified() => {
                    tracing::info!("swarm-term bridge: watcher cancelled");
                    return;
                }
                _ = tokio::time::sleep(Duration::from_millis(WATCH_POLL_MS)) => {}
            }
            continue;
        }
        if was_paused {
            // A message's grace (NOT_READY_GRACE / LOCKED_GRACE) is its
            // file age, which kept growing through the hold. Restart it
            // from the resume so a message held for minutes is not
            // rejected the instant its target is still warming up.
            reset_grace_clocks(&root);
            was_paused = false;
        }
        // The current agent → pane map (a restarted agent has a new pane).
        let panes_by_agent: Arc<HashMap<String, String>> =
            Arc::new(shared_panes.read().unwrap_or_else(|p| p.into_inner()).clone());
        process_pending(
            &app,
            &registry,
            &root,
            &panes_by_agent,
            &ready_panes,
            &lifecycle,
            &control,
            &mut stuck,
        )
        .await;
        if stall_every_read.elapsed() >= STALL_CADENCE_REFRESH {
            stall_every = stall_check_every();
            stall_every_read = Instant::now();
        }
        if last_stall_check.elapsed() >= stall_every {
            check_stalls(&app, &registry, &root, &panes_by_agent, &lifecycle).await;
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

#[allow(clippy::too_many_arguments)]
async fn process_pending<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    panes_by_agent: &Arc<HashMap<String, String>>,
    ready_panes: &Arc<Mutex<HashSet<String>>>,
    lifecycle: &Arc<LifecycleStore>,
    control: &Arc<super::BridgeControl>,
    stuck: &mut StuckMoves,
) {
    retry_stuck_moves(root, stuck);

    // Legacy `inbox/<target>/` drops (the pre-outbox protocol, where the
    // sender was taken from the file's own `from` field): never
    // delivered — rejected with a pointer to the sender's outbox.
    for &dir_agent in AGENT_IDS {
        for path in json_entries(&root.join("inbox").join(dir_agent)) {
            if stuck.contains_key(&path) {
                continue;
            }
            reject_legacy_inbox(app, registry, root, &path, dir_agent, panes_by_agent, stuck)
                .await;
        }
    }

    for (_, path, sender) in outbox_entries(root, stuck) {
        // A hold set mid-tick (the user's Pause, the budget or the usage
        // limit) stops the rest of this tick: each paste can take seconds,
        // and under the usage limit a late paste lands on claude's limit
        // menu. The remaining files wait, in order, for the next run.
        if control.is_paused() {
            return;
        }
        process_one(
            app,
            registry,
            root,
            &path,
            sender,
            panes_by_agent,
            ready_panes,
            lifecycle,
            stuck,
        )
        .await;
    }
}

/// The outbox files to process this tick. The SENDER is the directory
/// the file was written to. One FIFO across all senders by write time
/// (mtime; renames preserve it), path as the tie-break — agents pick
/// their own filenames, so name order is not chronological. Files that
/// settled on an earlier tick but are still waiting for their move are
/// left out.
fn outbox_entries(
    root: &Path,
    stuck: &StuckMoves,
) -> Vec<(Option<SystemTime>, PathBuf, &'static str)> {
    let mut entries: Vec<(Option<SystemTime>, PathBuf, &'static str)> = AGENT_IDS
        .iter()
        .flat_map(|&sender| {
            json_entries(&root.join("outbox").join(sender))
                .into_iter()
                .filter(move |p| !stuck.contains_key(p))
                .map(move |p| {
                    let mtime = fs::metadata(&p).and_then(|m| m.modified()).ok();
                    (mtime, p, sender)
                })
        })
        .collect();
    entries.sort();
    entries
}

/// `*.json` entries of `dir` (empty when it does not exist).
fn json_entries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect()
        })
        .unwrap_or_default()
}

/// Reject a file dropped under the legacy `inbox/<dir_agent>/` path and,
/// when its claimed sender can be read, tell that agent to use its
/// outbox. The claimed `from` is unverifiable here — it only selects who
/// receives the (harmless) notice.
async fn reject_legacy_inbox<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    path: &Path,
    dir_agent: &str,
    panes_by_agent: &HashMap<String, String>,
    stuck: &mut StuckMoves,
) {
    if let Err(reason) = file_gate(path) {
        reject_unread(app, root, path, dir_agent, &reason, stuck);
        return;
    }
    let raw = fs::read_to_string(path).unwrap_or_default();
    let sender = sniff_from(&raw);
    let reason = format!(
        "legacy inbox path — write messages to {}/outbox/<your-id>/ (sender is taken from that directory)",
        root.display()
    );
    let _ = app.emit(
        crate::events::SWARM_TERM_ROUTE,
        json!({
            "source": sender.as_deref().unwrap_or("?"),
            "target": dir_agent,
            "body": truncate(&raw, 200),
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
        sender.as_deref(),
        dir_agent,
        &reason,
        &raw,
        stuck,
    )
    .await;
}

/// Only a regular file of bounded size is ever read. `symlink_metadata`
/// does not follow links, so an entry pointing at e.g. ~/.ssh/id_rsa is
/// rejected unread instead of having its first bytes echoed into the
/// Routing Log; the size cap keeps a huge file from being loaded into
/// memory and then pasted into a pane. A missing file passes (the
/// following read reports the concurrent move).
fn file_gate(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            Err("not a regular file (symlink?)".into())
        }
        Ok(meta) if meta.len() > MAX_ENVELOPE_BYTES => Err(format!(
            "envelope too large ({} bytes > {MAX_ENVELOPE_BYTES})",
            meta.len()
        )),
        _ => Ok(()),
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

/// Set every pending outbox / legacy-inbox file's mtime to now, so the
/// delivery grace ([`waited`]) is measured from the resume rather than
/// from when the message was first written (which includes the hold).
fn reset_grace_clocks(root: &Path) {
    let now = SystemTime::now();
    for kind in ["outbox", "inbox"] {
        for &agent in AGENT_IDS {
            for path in json_entries(&root.join(kind).join(agent)) {
                if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
                    let _ = file.set_modified(now);
                }
            }
        }
    }
}

fn should_emit_retry_event(attempts: u32) -> bool {
    attempts == 1 || attempts % RETRY_EVENT_EVERY == 0
}

/// Move `path` to `rejected/` and, when the sender is a known agent,
/// paste a `[Neuron]` notice into the sender's pane. A file whose move
/// fails is remembered as settled ([`StuckMoves`]): it is never
/// processed — or noticed — again, only its move is retried.
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
    stuck: &mut StuckMoves,
) {
    let sender = sender.filter(|s| AGENT_IDS.contains(s));
    // `target` may be an unvalidated `to` value; it is only ever used as
    // a directory name when it is a known agent id (no `../` escapes).
    let bucket = if AGENT_IDS.contains(&target) {
        target
    } else {
        sender.unwrap_or(UNSORTED_BUCKET)
    };
    if let Err(e) = move_to_rejected(root, path, bucket, reason) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "swarm-term bridge: rejected-move failed — retrying the move only"
        );
        note_stuck(
            stuck,
            path,
            SettledDest::Rejected {
                bucket: bucket.to_string(),
                reason: reason.to_string(),
            },
        );
    }
    let Some(sender) = sender else {
        return;
    };
    let text = rejection_notice(sender, target, reason, body);
    notify_agent(registry, panes_by_agent, sender, &text).await;
    emit_notice(app, sender, &text, reason);
}

/// `rejected/` sub-directory for envelopes whose target is unknown and
/// whose sender could not be determined.
const UNSORTED_BUCKET: &str = "_unsorted";

/// Reject an entry WITHOUT reading it (it failed the file gate), filing
/// it under `rejected/<bucket>/` (the outbox sender, or the legacy inbox
/// directory). No notice: nothing was read to address one with. A failed
/// move is remembered as settled, like in [`reject_and_notify`].
fn reject_unread<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    path: &Path,
    bucket: &str,
    reason: &str,
    stuck: &mut StuckMoves,
) {
    tracing::warn!(
        path = %path.display(),
        reason = %reason,
        "swarm-term bridge: inbox entry rejected unread"
    );
    if let Err(e) = move_to_rejected(root, path, bucket, reason) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "swarm-term bridge: rejected-move failed — retrying the move only"
        );
        note_stuck(
            stuck,
            path,
            SettledDest::Rejected {
                bucket: bucket.to_string(),
                reason: reason.to_string(),
            },
        );
    }
    let _ = app.emit(
        crate::events::SWARM_TERM_ROUTE,
        json!({
            "source": bucket,
            "target": "?",
            "body": "",
            "outcome": "rejected",
            "reason": reason,
        }),
    );
}

/// Routing Log row for a `[Neuron]` notice pasted into `recipient`'s pane.
pub(crate) fn emit_notice<R: Runtime>(app: &AppHandle<R>, recipient: &str, text: &str, reason: &str) {
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
    sender: &'static str,
    panes_by_agent: &Arc<HashMap<String, String>>,
    ready_panes: &Arc<Mutex<HashSet<String>>>,
    lifecycle: &Arc<LifecycleStore>,
    stuck: &mut StuckMoves,
) {
    // ---- File gate --------------------------------------------------- //
    if let Err(reason) = file_gate(path) {
        reject_unread(app, root, path, sender, &reason, stuck);
        return;
    }

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
                let _ = app.emit(
                    crate::events::SWARM_TERM_ROUTE,
                    json!({
                        "source": sender,
                        "target": "?",
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
                    Some(sender),
                    "?",
                    &reason,
                    &raw,
                    stuck,
                )
                .await;
            }
            return;
        }
    };
    envelope.normalize();

    // The sender is the outbox the file was written to, never the
    // content's own claim. A `from` field that names someone else is a
    // (possibly accidental) spoof and is refused, not silently fixed.
    let mut to_agent = envelope.to.clone();
    let from_agent = sender.to_string();
    let body = envelope.body.clone();
    // The structured `task_id` is handed to the lifecycle layer as a
    // fallback id for bare-keyword bodies (see `lifecycle_synthesise`).
    let envelope_task_id = envelope.task_id.clone();

    // ---- Structural validation -------------------------------------- //
    let mut effective = envelope.clone();
    let spoofed = !effective.from.is_empty() && effective.from != sender;
    effective.from = from_agent.clone();
    let validation = if spoofed {
        Err(format!(
            "`from` is \"{}\" but the message was written to outbox/{sender}/ — \
             the sender is taken from the outbox directory",
            envelope.from
        ))
    } else {
        effective.validate()
    };
    if let Err(reason) = validation {
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
            stuck,
        )
        .await;
        return;
    }

    // ---- Misaddressed lifecycle signal ------------------------------ //
    // A `DONE` / verdict / task `hata` only drives the chain when the
    // coordinator receives it; one addressed elsewhere is re-addressed
    // there (the sender is told after delivery) instead of stalling.
    let redirected_from =
        misaddressed_signal(&from_agent, &to_agent, &body, envelope_task_id.as_deref())
            .then(|| std::mem::replace(&mut to_agent, "coordinator".to_string()));
    // A tracked dispatch that named its task only in the body still
    // carries that id (signature, Assigned tracking, task-id gate).
    let envelope_task_id = delivery_task_id(&from_agent, &to_agent, &body, envelope_task_id);

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
            stuck,
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
                stuck,
            )
            .await;
            return;
        }
    };

    // ---- Task-id gate ----------------------------------------------- //
    // Task ids are single-use and one open task belongs to one builder;
    // a dispatch that breaks that would corrupt the lifecycle.
    if let Some(reason) = dispatch_conflict(
        &from_agent,
        &to_agent,
        &target_pane,
        envelope_task_id.as_deref(),
        panes_by_agent,
        lifecycle,
    ) {
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
            &reason,
            &body,
            stuck,
        )
        .await;
        return;
    }

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
                stuck,
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
                stuck,
            )
            .await;
        }
        return;
    }

    // ---- Deliver ---------------------------------------------------- //
    let signed =
        format_routed_message(&body, &from_agent, envelope_task_id.as_deref());
    let registry_inner = registry.clone();
    let target_pane_for_write = target_pane.clone();
    let signed_bytes = signed.into_bytes();
    // Only the PASTE is the retried unit. The submitting CR is sent
    // afterwards and never causes a retry: once the text has landed,
    // re-delivering would paste it into the input box a second time.
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
            if let Err(e) = submit_pasted(registry, &target_pane).await {
                tracing::warn!(
                    target = %to_agent,
                    error = %e,
                    "swarm-term bridge: message pasted but submit CR failed; \
                     not re-pasting (text stays in the target's input box)"
                );
            }
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
                // Delivered: from here on only the move may be retried —
                // processing the file again would paste it twice and
                // re-run the lifecycle follow-ups below.
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "swarm-term bridge: processed-move failed — retrying the move only"
                );
                note_stuck(
                    stuck,
                    path,
                    SettledDest::Processed {
                        target: to_agent.clone(),
                    },
                );
            }
            let task_id = envelope_task_id.as_deref();
            note_dispatch(app, &from_agent, &to_agent, &target_pane, &body, task_id, lifecycle);
            lifecycle.note_message(&from_agent, &to_agent, task_id);
            note_coordinator_report(
                app,
                &from_agent,
                &to_agent,
                &body,
                task_id,
                panes_by_agent,
                lifecycle,
            );
            if let Some(original) = &redirected_from {
                let text = format!(
                    "`{}` mesajın @{original} yerine @coordinator'a iletildi: lifecycle \
                     sinyalleri (DONE / APPROVED / CHANGES_NEEDED / BUILDING, task_id'li \
                     `hata`) yalnızca coordinator'a gönderilir. Bundan sonra doğrudan \
                     coordinator'a yolla; @{original}'e ayrıca bir şey söyleyeceksen \
                     token'sız ayrı bir mesaj yaz.",
                    truncate(&body, 60)
                );
                notify_agent(registry, panes_by_agent, &from_agent, &text).await;
                emit_notice(app, &from_agent, &text, "lifecycle signal re-addressed to coordinator");
            }
            // Lifecycle synthesis runs only on coordinator-bound
            // deliveries — the bridge fans the autonomy follow-up out
            // (builder DONE -> reviewer "review", reviewer APPROVED ->
            // orchestrator "TASK_DONE" or — for a tester-rejected task —
            // tester "retest", hata / used-up rework limit ->
            // orchestrator "TASK_FAILED").
            if to_agent == "coordinator" {
                let notices = lifecycle_synthesise(
                    app,
                    root,
                    &from_agent,
                    &body,
                    task_id,
                    panes_by_agent,
                    lifecycle,
                );
                for n in notices {
                    tracing::info!(
                        to = %n.to,
                        reason = n.reason,
                        "swarm-term bridge: lifecycle notice"
                    );
                    notify_agent(registry, panes_by_agent, &n.to, &n.text).await;
                    emit_notice(app, &n.to, &n.text, n.reason);
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
                    stuck,
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm_term::bridge::prepare_layout;

    fn outbox_file(root: &Path, sender: &str, name: &str, body: &str) -> PathBuf {
        let path = root.join("outbox").join(sender).join(name);
        fs::write(&path, body).expect("write outbox file");
        path
    }

    /// Put a FILE where a destination directory goes, so moves into it fail.
    fn block(dir: &Path) {
        fs::remove_dir_all(dir).expect("rm dir");
        fs::write(dir, b"").expect("blocker");
    }

    fn unblock(dir: &Path) {
        fs::remove_file(dir).expect("rm blocker");
        fs::create_dir_all(dir).expect("mkdir");
    }

    #[test]
    fn reset_grace_clocks_moves_pending_mtimes_forward() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        let path = outbox_file(root, "scout", "m.json", r#"{"to":"orchestrator","body":"hi"}"#);
        // Backdate the message as if it had waited through a long hold.
        let old = SystemTime::now() - Duration::from_secs(3600);
        fs::OpenOptions::new().write(true).open(&path).unwrap().set_modified(old).unwrap();
        reset_grace_clocks(root);
        let m = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(m > old + Duration::from_secs(1800), "pending mtime was not reset on resume");
    }

    #[test]
    fn a_delivered_file_whose_move_fails_is_moved_later_never_reprocessed() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        let path = outbox_file(root, "scout", "m1.json", r#"{"to":"orchestrator","body":"hi"}"#);
        let processed = root.join("processed").join("orchestrator");
        block(&processed);

        // The delivery path: the move fails, the file is remembered.
        assert!(move_to_processed(root, &path, "orchestrator").is_err());
        let mut stuck = StuckMoves::new();
        note_stuck(&mut stuck, &path, SettledDest::Processed { target: "orchestrator".into() });

        // Still blocked: retried, kept, and left out of the next scan.
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.contains_key(&path));
        assert!(outbox_entries(root, &stuck).is_empty());

        // Unblocked: moved once, then forgotten.
        unblock(&processed);
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.is_empty());
        assert!(!path.exists());
        assert!(processed.join("m1.json").is_file());
        assert!(outbox_entries(root, &stuck).is_empty());
    }

    #[test]
    fn a_stuck_rejection_lands_with_its_reason() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        let path = outbox_file(root, "scout", "m2.json", r#"{"to":"backend-builder","body":"x"}"#);
        let rejected = root.join("rejected").join("backend-builder");
        block(&rejected);

        let dest = SettledDest::Rejected {
            bucket: "backend-builder".into(),
            reason: "hierarchy denies scout→backend-builder".into(),
        };
        assert!(settle(root, &path, &dest).is_err());
        let mut stuck = StuckMoves::new();
        note_stuck(&mut stuck, &path, dest);
        retry_stuck_moves(root, &mut stuck);
        assert_eq!(stuck.len(), 1);

        unblock(&rejected);
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.is_empty());
        assert!(rejected.join("m2.json").is_file());
        assert_eq!(
            fs::read_to_string(rejected.join("m2.reason")).expect("reason"),
            "hierarchy denies scout→backend-builder"
        );
    }

    #[test]
    fn a_rewritten_or_vanished_file_leaves_the_stuck_set() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        block(&root.join("processed").join("orchestrator"));
        let rewritten = outbox_file(root, "scout", "a.json", r#"{"to":"orchestrator","body":"1"}"#);
        let vanished = outbox_file(root, "planner", "b.json", r#"{"to":"orchestrator","body":"2"}"#);
        let mut stuck = StuckMoves::new();
        for p in [&rewritten, &vanished] {
            note_stuck(&mut stuck, p, SettledDest::Processed { target: "orchestrator".into() });
        }

        // A NEW message under the same name, and a file someone removed.
        fs::write(&rewritten, r#"{"to":"orchestrator","body":"a newer, longer message"}"#)
            .expect("rewrite");
        fs::remove_file(&vanished).expect("rm");
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.is_empty());
        // The rewritten file is processed again as the new message it is.
        let pending: Vec<PathBuf> =
            outbox_entries(root, &stuck).into_iter().map(|(_, p, _)| p).collect();
        assert_eq!(pending, vec![rewritten]);
    }

    #[test]
    fn a_file_already_gone_is_dropped_by_the_first_retry() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        let mut stuck = StuckMoves::new();
        note_stuck(
            &mut stuck,
            &root.join("outbox").join("scout").join("gone.json"),
            SettledDest::Processed { target: "orchestrator".into() },
        );
        assert_eq!(stuck.len(), 1, "remembered even without metadata");
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.is_empty(), "the move reports NotFound, so it is dropped");
    }

    /// The lock that failed the move can also fail the stat: such an
    /// entry is still never reprocessed, adopts the first readable
    /// stamp, and is moved once the lock clears.
    #[test]
    fn an_unreadable_stamp_still_blocks_reprocessing() {
        let d = tempfile::tempdir().expect("tempdir");
        let root = d.path();
        prepare_layout(root).expect("layout");
        let path = outbox_file(root, "scout", "m3.json", r#"{"to":"orchestrator","body":"hi"}"#);
        let processed = root.join("processed").join("orchestrator");
        block(&processed);
        let mut stuck = StuckMoves::new();
        stuck.insert(
            path.clone(),
            StuckMove {
                dest: SettledDest::Processed { target: "orchestrator".into() },
                stamp: None,
            },
        );
        assert!(outbox_entries(root, &stuck).is_empty());
        retry_stuck_moves(root, &mut stuck);
        assert_eq!(stuck[&path].stamp, file_stamp(&path), "stamp adopted");
        unblock(&processed);
        retry_stuck_moves(root, &mut stuck);
        assert!(stuck.is_empty());
        assert!(processed.join("m3.json").is_file());
    }
}

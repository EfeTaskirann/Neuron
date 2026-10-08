//! Filesystem layout + durable-queue operations for the bridge tree.
//!
//! The bridge uses the file system itself as the queue: `inbox/` holds
//! not-yet-delivered envelopes, `processed/` holds delivered ones, and
//! `rejected/` holds permanently failed ones (with a `.reason`
//! sidecar). Atomic `.tmp`+rename writes and renames keep those
//! invariants crash-safe. A per-file `.attempts` sidecar bounds retries.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ulid::Ulid;

use crate::error::AppError;
use crate::swarm_term::hierarchy::AGENT_IDS;

use super::envelope::Envelope;

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

/// Directory (under the bridge parent, `<project>/.bridgespace/`) that
/// keeps the `rejected/` evidence of stopped sessions.
const ARCHIVE_DIRNAME: &str = "_archive";

/// How many stopped sessions' rejection evidence is kept. Session dirs
/// are `swarm-term-<ULID>`, so name order is chronological and the
/// oldest are pruned first.
const MAX_ARCHIVED_SESSIONS: usize = 20;

/// Move a stopping session's `rejected/` tree (envelopes + `.reason`
/// sidecars) to `<bridge parent>/_archive/<session>/rejected` before
/// the session root is deleted, so the evidence of why messages failed
/// survives the session. No-op (returns `Ok(None)`) when nothing was
/// rejected. Prunes the archive to the newest [`MAX_ARCHIVED_SESSIONS`].
pub fn archive_rejected(session_root: &Path) -> io::Result<Option<PathBuf>> {
    let rejected = session_root.join("rejected");
    let has_files = fs::read_dir(&rejected)
        .map(|agents| {
            agents.filter_map(Result::ok).any(|agent_dir| {
                fs::read_dir(agent_dir.path())
                    .map(|mut files| files.next().is_some())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false);
    if !has_files {
        return Ok(None);
    }
    let (Some(parent), Some(session)) =
        (session_root.parent(), session_root.file_name())
    else {
        return Err(io::Error::other("session root has no parent/name"));
    };
    let archive = parent.join(ARCHIVE_DIRNAME);
    let dest_session = archive.join(session);
    fs::create_dir_all(&dest_session)?;
    let dest = dest_session.join("rejected");
    fs::rename(&rejected, &dest)?;

    let mut sessions: Vec<PathBuf> = fs::read_dir(&archive)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    sessions.sort();
    let excess = sessions.len().saturating_sub(MAX_ARCHIVED_SESSIONS);
    for old in sessions.into_iter().take(excess) {
        let _ = fs::remove_dir_all(old);
    }
    Ok(Some(dest))
}

/// Remove `swarm-term-*` session trees left under `bridge_parent` by a
/// session that never ran `stop()` (app killed by a `tauri dev` rebuild,
/// a crash, a forced quit). Their `rejected/` evidence is archived first,
/// exactly as `stop()` would. Only call when no session is running —
/// every such tree is then an orphan. Returns how many were removed.
pub fn sweep_orphan_sessions(bridge_parent: &Path) -> usize {
    let Ok(entries) = fs::read_dir(bridge_parent) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_session = path.is_dir()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("swarm-term-"));
        if !is_session {
            continue;
        }
        if let Err(e) = archive_rejected(&path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "swarm-term: orphan session — rejected-archive failed"
            );
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "swarm-term: orphan session cleanup failed"
            ),
        }
    }
    removed
}

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
pub(super) fn bump_attempts(path: &Path) -> u32 {
    let sidecar = attempts_sidecar(path);
    let cur: u32 = fs::read_to_string(&sidecar)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let next = cur.saturating_add(1);
    let _ = fs::write(&sidecar, next.to_string());
    next
}

pub(super) fn attempts_sidecar(path: &Path) -> PathBuf {
    let mut sidecar = path.to_path_buf();
    sidecar.set_extension("attempts");
    sidecar
}

pub(super) fn move_to_processed(
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

pub(super) fn move_to_rejected(
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

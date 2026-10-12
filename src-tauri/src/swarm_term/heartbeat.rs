//! Liveness marker for a running session's on-disk trees.
//!
//! A running session refreshes a small marker file in its pane-HOME
//! root and its `.bridgespace/<session>/` root every
//! [`HEARTBEAT_EVERY`]. The orphan sweep (run when a session starts)
//! skips any tree whose marker is fresher than [`HEARTBEAT_STALE_AFTER`]:
//! "no session is running in THIS process" does not mean no session is
//! running — a second Neuron instance (dev build + installed app) shares
//! the same app-data dir and may share the project dir. A crashed or
//! killed session stops beating, so its trees become sweepable.
//!
//! An mtime heartbeat instead of a file lock keeps this dependency-free
//! and inside the crate's MSRV (`File::try_lock` is 1.89+).

use std::path::Path;
use std::time::{Duration, SystemTime};

const HEARTBEAT_FILE: &str = ".neuron-heartbeat";

/// How often a running session refreshes its markers.
pub(crate) const HEARTBEAT_EVERY: Duration = Duration::from_secs(20);

/// A marker older than this means the owning session is gone. Several
/// missed beats of headroom so a briefly stalled runtime is not swept.
const HEARTBEAT_STALE_AFTER: Duration = Duration::from_secs(90);

/// Refresh `dir`'s marker (best-effort).
pub(crate) fn beat(dir: &Path) {
    let _ = std::fs::write(dir.join(HEARTBEAT_FILE), std::process::id().to_string());
}

/// True when `dir` belongs to a session that is still beating. Trees
/// without a marker (older builds, crashed before the first beat) count
/// as not live.
pub(crate) fn is_live(dir: &Path) -> bool {
    is_live_at(dir, SystemTime::now())
}

fn is_live_at(dir: &Path, now: SystemTime) -> bool {
    std::fs::metadata(dir.join(HEARTBEAT_FILE))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| now.duration_since(t).ok().or(Some(Duration::ZERO)))
        .is_some_and(|age| age < HEARTBEAT_STALE_AFTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_marker_is_live_stale_or_missing_is_not() {
        let d = tempfile::tempdir().expect("tempdir");
        assert!(!is_live(d.path()), "no marker → not live");
        beat(d.path());
        assert!(is_live(d.path()));
        let later = SystemTime::now() + HEARTBEAT_STALE_AFTER + Duration::from_secs(1);
        assert!(!is_live_at(d.path(), later), "stale marker → not live");
    }
}

//! Per-pane HOME isolation for the terminal swarm.
//!
//! Each of the 9 claude REPLs gets a private HOME (under
//! `app_data_dir/swarm-term/homes/<session_id>/<agent>/`) seeded with
//! copies of the user's `~/.claude.json` + `~/.claude/` top-level files
//! (especially `.credentials.json`). This stops 9 parallel `claude.exe`
//! processes from racing on the user's real `~/.claude.json` and
//! truncating it to invalid JSON. Extracted from `session.rs` so the
//! spawn/teardown flow there stays focused on session lifecycle.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::swarm_term::heartbeat;

/// Create the per-session HOME isolation root under
/// `app_data_dir/swarm-term/homes/<session_id>/`. The directory is
/// fresh each session — no carry-over between sessions, no cleanup
/// race against running panes.
pub(crate) fn prepare_isolated_homes_root<R: Runtime>(
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

/// Delete pane-HOME trees left by sessions that never ran `stop()` (app
/// killed by a `tauri dev` rebuild, a crash). Each holds a copy of the
/// user's credentials, so they must not accumulate — and one of them may
/// hold the only fresh, rotated refresh token, so credentials are synced
/// out of each tree before it goes. Trees still beating (a session of
/// ANOTHER Neuron instance sharing this app-data dir — see
/// [`crate::swarm_term::heartbeat`]) are skipped. Call only when this
/// process runs no session. Returns how many trees were removed.
pub(crate) fn sweep_orphan_homes<R: Runtime>(app: &AppHandle<R>) -> usize {
    let Ok(app_data) = app.path().app_data_dir() else {
        return 0;
    };
    sweep_orphan_homes_in(
        &app_data.join("swarm-term").join("homes"),
        real_credentials_path().as_deref(),
    )
}

fn sweep_orphan_homes_in(homes_base: &Path, real_credentials: Option<&Path>) -> usize {
    let Ok(entries) = std::fs::read_dir(homes_base) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || heartbeat::is_live(&path) {
            continue;
        }
        sync_credentials_with(real_credentials, &path);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "swarm-term: orphan pane-homes cleanup failed"
            ),
        }
    }
    removed
}

/// Seed a per-pane HOME directory by copying the user's real
/// `~/.claude.json` + `~/.claude/.credentials.json` into the pane's
/// isolated home.
pub(crate) fn seed_pane_home(
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

    if real_claude_json.is_file() {
        std::fs::copy(&real_claude_json, pane_home.join(".claude.json"))
            .map_err(|e| {
                AppError::Internal(format!(
                    "copy {} → pane: {e}",
                    real_claude_json.display()
                ))
            })?;
    }

    let pane_claude_dir = pane_home.join(".claude");
    std::fs::create_dir_all(&pane_claude_dir).map_err(|e| {
        AppError::Internal(format!(
            "mkdir {}: {e}",
            pane_claude_dir.display()
        ))
    })?;

    let real_credentials = real_claude_dir.join(".credentials.json");
    let pane_credentials = pane_claude_dir.join(".credentials.json");
    if real_credentials.is_file() {
        copy_credentials_with_retry(&real_credentials, &pane_credentials)
            .map_err(|e| {
                AppError::Internal(format!(
                    "pane `{agent_id}` could not seed .credentials.json \
                     from {} — claude.exe in this pane would drop into \
                     /login at first prompt. Error: {e}",
                    real_credentials.display(),
                ))
            })?;
    }

    if real_claude_dir.is_dir() {
        if let Ok(read) = std::fs::read_dir(&real_claude_dir) {
            for entry in read.flatten() {
                let src = entry.path();
                if src == real_credentials {
                    continue;
                }
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

/// Copy `.credentials.json` from the host home into a pane home, with
/// one retry on transient failure and a post-condition check that the
/// destination is non-empty.
fn copy_credentials_with_retry(
    src: &Path,
    dst: &Path,
) -> std::io::Result<()> {
    let src_len = src.metadata()?.len();
    let attempt = |first: bool| -> std::io::Result<()> {
        let _ = std::fs::remove_file(dst); // ignore not-found on first try
        let copied = std::fs::copy(src, dst)?;
        if src_len > 0 && copied < src_len {
            return Err(std::io::Error::other(format!(
                "{} partial copy: wrote {copied} of {src_len} bytes (attempt {})",
                dst.display(),
                if first { "1" } else { "2" },
            )));
        }
        Ok(())
    };
    match attempt(true) {
        Ok(()) => Ok(()),
        Err(e1) => {
            std::thread::sleep(std::time::Duration::from_millis(50));
            attempt(false).map_err(|e2| {
                std::io::Error::new(
                    e2.kind(),
                    format!("first attempt: {e1}; retry: {e2}"),
                )
            })
        }
    }
}

/// Key of the claude.ai OAuth block inside `.credentials.json`.
const OAUTH_KEY: &str = "claudeAiOauth";

/// Propagate the freshest claude OAuth credentials between the user's
/// real `~/.claude/.credentials.json` and every pane HOME under
/// `homes_root`. Returns how many files were updated.
///
/// Why: the refresh token is single-use (rotated on refresh). With 9
/// isolated copies, the first pane to refresh rotates it and every
/// other copy — including the user's real file, which is never written
/// back — is left holding a dead token: the next session (or the other
/// panes, once their access token expires) drops into `/login`. Normal
/// claude processes share one file and pick up each other's refreshes;
/// syncing restores that.
pub(crate) fn sync_credentials(homes_root: &Path) -> usize {
    sync_credentials_with(real_credentials_path().as_deref(), homes_root)
}

fn real_credentials_path() -> Option<PathBuf> {
    real_user_home()
        .ok()
        .map(|home| home.join(".claude").join(".credentials.json"))
}

/// [`sync_credentials`] with the user's real credentials file passed in
/// explicitly — tests pass `None` / a temp file so they can never touch
/// the developer's real `~/.claude/.credentials.json`.
fn sync_credentials_with(real: Option<&Path>, homes_root: &Path) -> usize {
    let mut paths: Vec<PathBuf> = real.map(Path::to_path_buf).into_iter().collect();
    if let Ok(panes) = std::fs::read_dir(homes_root) {
        for pane in panes.flatten() {
            paths.push(pane.path().join(".claude").join(".credentials.json"));
        }
    }
    sync_credential_files(&paths)
}

/// Core of [`sync_credentials`] over an explicit file list: pick the
/// file whose `claudeAiOauth.expiresAt` is latest and copy its
/// `claudeAiOauth` block into every file with an older one, keeping
/// each file's other keys. Missing, unreadable or non-JSON files are
/// never written (one may be mid-write by claude); writes are atomic.
fn sync_credential_files(paths: &[PathBuf]) -> usize {
    let parsed: Vec<(&PathBuf, serde_json::Value)> = paths
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            let v: serde_json::Value = serde_json::from_str(&text).ok()?;
            v.is_object().then_some((p, v))
        })
        .collect();
    let expiry = |v: &serde_json::Value| {
        v.get(OAUTH_KEY)?.get("expiresAt")?.as_i64()
    };
    let Some((newest_path, newest)) = parsed
        .iter()
        .filter(|(_, v)| {
            expiry(v).is_some()
                && v.get(OAUTH_KEY)
                    .and_then(|o| o.get("refreshToken"))
                    .is_some_and(|t| t.as_str().is_some_and(|s| !s.is_empty()))
        })
        .max_by_key(|(_, v)| expiry(v))
    else {
        return 0;
    };
    let newest_expiry = expiry(newest);
    let Some(oauth) = newest.get(OAUTH_KEY).cloned() else {
        return 0;
    };
    let mut updated = 0;
    for (path, value) in &parsed {
        if *path == *newest_path || expiry(value) >= newest_expiry {
            continue;
        }
        let mut merged = value.clone();
        merged[OAUTH_KEY] = oauth.clone();
        match write_atomic(path, &merged.to_string()) {
            Ok(()) => updated += 1,
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "swarm-term: credentials sync write failed (will retry)"
            ),
        }
    }
    if updated > 0 {
        tracing::info!(
            source = %newest_path.display(),
            updated,
            "swarm-term: propagated refreshed claude credentials"
        );
    }
    updated
}

fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".neuron-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(expires: i64, refresh: &str, extra: &str) -> String {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"a{expires}","refreshToken":"{refresh}","expiresAt":{expires}}}{extra}}}"#
        )
    }

    fn oauth_of(p: &Path) -> serde_json::Value {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
        v["claudeAiOauth"].clone()
    }

    #[test]
    fn sync_propagates_the_freshest_oauth_and_keeps_other_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real.json");
        let pane_a = tmp.path().join("a.json");
        let pane_b = tmp.path().join("b.json");
        // Real file is stale but carries an extra key that must survive.
        std::fs::write(&real, creds(100, "old", r#","mcpOAuth":{"k":1}"#)).unwrap();
        std::fs::write(&pane_a, creds(500, "rotated", "")).unwrap();
        std::fs::write(&pane_b, creds(100, "old", "")).unwrap();

        let n = sync_credential_files(&[real.clone(), pane_a.clone(), pane_b.clone()]);

        assert_eq!(n, 2);
        assert_eq!(oauth_of(&real)["refreshToken"], "rotated");
        assert_eq!(oauth_of(&pane_b)["expiresAt"], 500);
        let real_v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(real_v["mcpOAuth"]["k"], 1, "other keys must be preserved");
        // Idempotent.
        assert_eq!(sync_credential_files(&[real, pane_a, pane_b]), 0);
    }

    #[test]
    fn sync_never_writes_unreadable_files_or_copies_tokenless_blocks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let partial = tmp.path().join("partial.json");
        let fresh_no_refresh = tmp.path().join("norefresh.json");
        let stale = tmp.path().join("stale.json");
        std::fs::write(&partial, r#"{"claudeAiOauth":{"acc"#).unwrap();
        std::fs::write(&fresh_no_refresh, creds(900, "", "")).unwrap();
        std::fs::write(&stale, creds(100, "old", "")).unwrap();
        let missing = tmp.path().join("missing.json");

        let n = sync_credential_files(&[
            partial.clone(),
            fresh_no_refresh,
            stale.clone(),
            missing.clone(),
        ]);

        assert_eq!(n, 0, "a block without a refresh token is not a valid source");
        assert_eq!(
            std::fs::read_to_string(&partial).unwrap(),
            r#"{"claudeAiOauth":{"acc"#,
            "a possibly mid-write file must never be overwritten"
        );
        assert_eq!(oauth_of(&stale)["refreshToken"], "old");
        assert!(!missing.exists(), "missing files are not created");
    }

    #[test]
    fn sweep_rescues_the_fresh_token_then_removes_orphan_homes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real-credentials.json");
        std::fs::write(&real, creds(100, "dead", "")).unwrap();
        let base = tmp.path().join("homes");
        // Orphan session A: one pane refreshed (rotated) its token.
        let a = base.join("swarm-term-A").join("orchestrator").join(".claude");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join(".credentials.json"), creds(900, "rotated", "")).unwrap();
        // Orphan session B: stale copy only.
        let b = base.join("swarm-term-B").join("scout").join(".claude");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join(".credentials.json"), creds(100, "dead", "")).unwrap();

        // A live session of another Neuron instance must be left alone.
        let live = base.join("swarm-term-LIVE");
        std::fs::create_dir_all(live.join("scout")).unwrap();
        crate::swarm_term::heartbeat::beat(&live);

        let removed = sweep_orphan_homes_in(&base, Some(&real));

        assert_eq!(removed, 2);
        assert!(live.is_dir(), "a beating session tree must not be swept");
        std::fs::remove_dir_all(&live).unwrap();
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
        assert_eq!(
            oauth_of(&real)["refreshToken"],
            "rotated",
            "the only live refresh token must survive the sweep"
        );
    }

    #[test]
    fn credentials_copy_writes_full_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join(".credentials.json");
        let dst = tmp.path().join("dst").join(".credentials.json");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let payload = br#"{"claudeAiOauth":{"accessToken":"tok"}}"#;
        std::fs::write(&src, payload).unwrap();

        copy_credentials_with_retry(&src, &dst).expect("copy");

        let got = std::fs::read(&dst).expect("read dst");
        assert_eq!(got, payload, "destination must match source byte-for-byte");
    }

    #[test]
    fn credentials_copy_errors_when_source_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("does-not-exist");
        let dst = tmp.path().join(".credentials.json");

        let err = copy_credentials_with_retry(&src, &dst)
            .expect_err("missing source must error");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::NotFound,
            "missing source must report NotFound, got: {err}"
        );
        assert!(
            !dst.is_file(),
            "destination must not be left behind when source is missing"
        );
    }

    #[test]
    fn credentials_copy_overwrites_pre_existing_destination() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join(".credentials.json");
        let dst = tmp.path().join("dst").join(".credentials.json");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, b"STALE").unwrap();
        let fresh = br#"{"claudeAiOauth":{"accessToken":"fresh"}}"#;
        std::fs::write(&src, fresh).unwrap();

        copy_credentials_with_retry(&src, &dst).expect("copy");

        let got = std::fs::read(&dst).expect("read dst");
        assert_eq!(got, fresh, "stale dst must be overwritten with fresh src");
    }

    #[test]
    fn seed_pane_home_propagates_credentials_failure() {
        let homes_root = tempfile::tempdir().expect("tempdir");
        let fake_home = tempfile::tempdir().expect("fake home");
        let real_claude = fake_home.path().join(".claude");
        std::fs::create_dir_all(&real_claude).unwrap();
        let creds = br#"{"claudeAiOauth":{"accessToken":"x"}}"#;
        std::fs::write(real_claude.join(".credentials.json"), creds).unwrap();

        let prior_home = std::env::var_os("HOME");
        let prior_profile = std::env::var_os("USERPROFILE");
        std::env::set_var("HOME", fake_home.path());
        std::env::set_var("USERPROFILE", fake_home.path());

        let result = seed_pane_home(homes_root.path(), "scout");

        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prior_profile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }

        let pane_home = result.expect("seed");
        let pane_creds = pane_home.join(".claude").join(".credentials.json");
        assert!(
            pane_creds.is_file(),
            "seed must place .credentials.json in pane home — claude.exe \
             reads this to skip /login"
        );
        let got = std::fs::read(&pane_creds).expect("read pane creds");
        assert_eq!(got, creds, "pane credentials must match host's");
    }
}

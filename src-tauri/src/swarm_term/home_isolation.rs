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

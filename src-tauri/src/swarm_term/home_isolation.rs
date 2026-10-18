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
    let settings = pane_claude_dir.join("settings.json");
    if strip_pane_only_settings(&settings) {
        tracing::debug!(agent_id = %agent_id, "swarm-term: pane settings — fullscreen TUI off");
    }
    Ok(pane_home)
}

/// Settings keys the pane's copy drops: `tui` (e.g. `"fullscreen"`).
/// The fullscreen TUI draws with cursor moves and almost no newlines, so
/// the pane's line capture stayed empty — the session could not see a
/// trust / login screen, nor tell when the REPL was up, and every
/// kickoff decision was blind. Panes always use the line-based TUI; the
/// user's own setting is untouched.
const PANE_DROPPED_SETTINGS: &[&str] = &["tui"];

/// Drop [`PANE_DROPPED_SETTINGS`] from a pane's `settings.json` copy.
/// Returns whether the file changed.
fn strip_pane_only_settings(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    let before = obj.len();
    obj.retain(|k, _| !PANE_DROPPED_SETTINGS.contains(&k.as_str()));
    if obj.len() == before {
        return false;
    }
    serde_json::to_string_pretty(&v)
        .ok()
        .is_some_and(|json| write_atomic(path, &json).is_ok())
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

/// How [`ensure_project_trust`] settled the folder-trust screen for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PaneTrust {
    /// The user's config already trusted the folder (or an ancestor):
    /// that decision was copied onto every path spelling.
    Mirrored,
    /// Never trusted, but the folder holds no project config that could
    /// run code (no `.claude/settings*.json`, no `.mcp.json`): trusted
    /// for the pane, since the screen guards nothing here.
    Granted,
    /// Left as is — the pane shows the trust screen for the user.
    Untouched,
}

/// Settle claude's "Is this a project you trust?" screen for a pane, in
/// the pane's `.claude.json` copy (the user's real config is never
/// touched), under every spelling claude may look the folder up with.
///
/// * The user already trusted `project` or an ancestor (any spelling):
///   mirror it. claude 2.1.29x keys projects by a lower-case drive
///   letter (`c:/…`) while older builds wrote `C:/…`, so a trusted folder
///   looked untrusted and every pane opened on the trust screen.
/// * Never trusted: the screen exists so a repository's own settings
///   (hooks, MCP servers) do not run unannounced. When the folder has
///   none, there is nothing to guard — trust it rather than make the
///   user answer the same screen in nine panes (a new empty project
///   folder hit exactly that, and the agents never became ready).
/// * Otherwise the pane keeps the screen for the user to decide.
pub(crate) fn ensure_project_trust(pane_home: &Path, project: &Path) -> PaneTrust {
    let path = pane_home.join(".claude.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return PaneTrust::Untouched;
    };
    let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return PaneTrust::Untouched;
    };
    let project_str = project.display().to_string();
    let outcome = if is_trusted_in(&cfg, &project_str) {
        PaneTrust::Mirrored
    } else if project_config_is_inert(project) {
        PaneTrust::Granted
    } else {
        return PaneTrust::Untouched;
    };
    set_trusted_in(&mut cfg, &project_str);
    match serde_json::to_string_pretty(&cfg) {
        Ok(json) if write_atomic(&path, &json).is_ok() => outcome,
        _ => PaneTrust::Untouched,
    }
}

/// True when `project` carries no Claude project config that executes
/// anything on its own (settings with hooks / commands, MCP servers).
fn project_config_is_inert(project: &Path) -> bool {
    ![
        project.join(".claude").join("settings.json"),
        project.join(".claude").join("settings.local.json"),
        project.join(".mcp.json"),
    ]
    .iter()
    .any(|p| p.exists())
}

/// Spelling-independent project key: `/` separators, lower case, no
/// trailing separator.
fn project_key(p: &str) -> String {
    p.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

/// True when the config trusts `project` or one of its ancestors, under
/// any spelling.
fn is_trusted_in(cfg: &serde_json::Value, project: &str) -> bool {
    let wanted = project_key(project);
    cfg.get("projects")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|projects| {
            projects.iter().any(|(key, entry)| {
                let key = project_key(key);
                entry.get("hasTrustDialogAccepted") == Some(&serde_json::Value::Bool(true))
                    && (wanted == key || wanted.starts_with(&format!("{key}/")))
            })
        })
}

/// Mark `project` trusted under its `/` spelling and both drive-letter
/// cases, keeping any other fields of existing entries.
fn set_trusted_in(cfg: &mut serde_json::Value, project: &str) {
    use serde_json::Value;
    let Some(root) = cfg.as_object_mut() else {
        return;
    };
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(projects) = projects.as_object_mut() else {
        return;
    };
    let forward = project.replace('\\', "/").trim_end_matches('/').to_string();
    let mut spellings = vec![forward.clone()];
    if forward.as_bytes().get(1) == Some(&b':') {
        let (drive, rest) = forward.split_at(1);
        for d in [drive.to_ascii_uppercase(), drive.to_ascii_lowercase()] {
            let s = format!("{d}{rest}");
            if !spellings.contains(&s) {
                spellings.push(s);
            }
        }
    }
    for key in spellings {
        let entry = projects
            .entry(key)
            .or_insert_with(|| Value::Object(Default::default()));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
        }
    }
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
pub(crate) fn real_user_home() -> Result<PathBuf, AppError> {
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

    /// The old mirror-only behaviour, on an in-memory config.
    fn mirror_trust_in(cfg: &mut serde_json::Value, project: &str) -> bool {
        let trusted = is_trusted_in(cfg, project);
        if trusted {
            set_trusted_in(cfg, project);
        }
        trusted
    }

    #[test]
    fn trust_is_mirrored_onto_every_drive_spelling_of_a_trusted_tree() {
        // Live: the user's config trusted `C:/…/Neuron` but also held a
        // `c:/…/Neuron: false` entry, which claude 2.1.29x reads.
        let mut cfg = serde_json::json!({ "projects": {
            "C:/Users/u/Neuron": { "hasTrustDialogAccepted": true, "allowedTools": [] },
            "c:/Users/u/Neuron": { "hasTrustDialogAccepted": false, "keep": 1 }
        }});
        assert!(mirror_trust_in(&mut cfg, "C:\\Users\\u\\Neuron\\.livetest\\inv"));
        let p = &cfg["projects"];
        assert_eq!(p["C:/Users/u/Neuron/.livetest/inv"]["hasTrustDialogAccepted"], true);
        assert_eq!(p["c:/Users/u/Neuron/.livetest/inv"]["hasTrustDialogAccepted"], true);
        // The project itself: its false spelling flips, other fields stay.
        assert!(mirror_trust_in(&mut cfg, "C:/Users/u/Neuron"));
        assert_eq!(cfg["projects"]["c:/Users/u/Neuron"]["hasTrustDialogAccepted"], true);
        assert_eq!(cfg["projects"]["c:/Users/u/Neuron"]["keep"], 1);
    }

    #[test]
    fn no_trust_is_granted_that_the_user_never_gave() {
        let mut cfg = serde_json::json!({ "projects": {
            "C:/Users/u/Neuro": { "hasTrustDialogAccepted": true },
            "C:/Users/u/Other": { "hasTrustDialogAccepted": false }
        }});
        let before = cfg.clone();
        // `Neuro` is not an ancestor of `Neuron` (no prefix trap).
        assert!(!mirror_trust_in(&mut cfg, "C:/Users/u/Neuron"));
        assert!(!mirror_trust_in(&mut cfg, "C:/Users/u/Other/sub"));
        assert!(!mirror_trust_in(&mut serde_json::json!({}), "C:/x"));
        assert_eq!(cfg, before);
        // POSIX paths have a single spelling.
        let mut cfg = serde_json::json!({ "projects": { "/home/u/p": { "hasTrustDialogAccepted": true } } });
        assert!(mirror_trust_in(&mut cfg, "/home/u/p/sub/"));
        assert_eq!(cfg["projects"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn mirror_project_trust_rewrites_only_the_pane_copy() {
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join(".claude.json");
        std::fs::write(&file, r#"{"projects":{"D:/w":{"hasTrustDialogAccepted":true}},"x":1}"#)
            .expect("write");
        assert_eq!(ensure_project_trust(home.path(), Path::new("D:/w/proj")), PaneTrust::Mirrored);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(v["projects"]["d:/w/proj"]["hasTrustDialogAccepted"], true);
        assert_eq!(v["x"], 1);
        // No config → nothing to do.
        let empty = tempfile::tempdir().expect("tempdir");
        assert_eq!(ensure_project_trust(empty.path(), Path::new("D:/w/proj")), PaneTrust::Untouched);
    }

    #[test]
    fn pane_settings_drop_the_fullscreen_tui_and_keep_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("settings.json");
        std::fs::write(&file, r#"{"tui":"fullscreen","theme":"dark","effortLevel":"xhigh"}"#)
            .expect("write");
        assert!(strip_pane_only_settings(&file));
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(v.get("tui").is_none());
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["effortLevel"], "xhigh");
        // Nothing to drop / no file → unchanged.
        assert!(!strip_pane_only_settings(&file));
        assert!(!strip_pane_only_settings(&dir.path().join("missing.json")));
    }

    #[test]
    fn a_new_folder_without_runnable_config_is_trusted_for_the_panes() {
        // Live: a brand-new project folder put the trust screen in all
        // nine panes; the agents never became ready.
        let home = tempfile::tempdir().expect("tempdir");
        let file = home.path().join(".claude.json");
        std::fs::write(&file, r#"{"projects":{}}"#).expect("write");
        let project = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(project.path().join(".claude")).expect("mkdir");
        std::fs::write(project.path().join(".claude").join("launch.json"), "{}").expect("w");
        assert_eq!(ensure_project_trust(home.path(), project.path()), PaneTrust::Granted);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(is_trusted_in(&v, &project.path().display().to_string()));
    }

    #[test]
    fn a_folder_with_runnable_project_config_keeps_the_trust_screen() {
        for runnable in [".claude/settings.json", ".claude/settings.local.json", ".mcp.json"] {
            let home = tempfile::tempdir().expect("tempdir");
            let file = home.path().join(".claude.json");
            std::fs::write(&file, r#"{"projects":{}}"#).expect("write");
            let project = tempfile::tempdir().expect("project");
            let p = project.path().join(runnable);
            std::fs::create_dir_all(p.parent().unwrap()).expect("mkdir");
            std::fs::write(&p, "{}").expect("w");
            assert_eq!(
                ensure_project_trust(home.path(), project.path()),
                PaneTrust::Untouched,
                "{runnable}"
            );
            assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"projects":{}}"#);
        }
    }

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

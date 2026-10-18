//! How the nine swarm-term panes run claude: on the user's own Claude
//! Code configuration, with nothing copied out of it.
//!
//! Every pane is the unmodified `claude` CLI with the user's own login.
//! The configuration directory (`~/.claude`, `~/.claude.json`) is shared
//! by all nine, like nine terminals the user opened themselves; Claude
//! Code coordinates its own token renewal across parallel sessions.
//! Neuron never reads, copies or writes a credential file (the old
//! per-pane HOME held a copy of `.credentials.json` and synced refreshed
//! tokens back, which Anthropic's terms do not allow a product to do).
//! What must not carry over from the user's own setup is kept out by
//! flags rather than by a trimmed copy:
//!
//!   * `--setting-sources project` — the user's own settings files
//!     (global, and the project-local `.claude/settings.local.json`:
//!     hooks, permission rules, model, plugins, the `tui` switch) do not
//!     load; the project's committed settings and CLAUDE.md still do;
//!   * `--strict-mcp-config` — no MCP server starts (no nine copies of
//!     the user's servers, no claude.ai connectors);
//!   * `CLAUDE_CODE_DISABLE_AUTO_MEMORY=1` — agents neither read nor
//!     write the user's auto-memory for the project;
//!   * `--session-id <uuid>` — each pane's transcript has a known name
//!     (`<projects>/<cwd-slug>/<uuid>.jsonl`), so readiness is read from
//!     it and `stop()` deletes exactly these transcripts.
//!
//! Known side effect: what the panes are sent (kickoff, routed messages)
//! lands in the user's prompt history for the project, as it would for
//! any claude session they ran there.
//!
//! Neuron's own per-session files (persona prompts, the pane manifest,
//! the heartbeat) live in `app_data/swarm-term/sessions/<session_id>/`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::swarm_term::heartbeat;

/// `app_data/swarm-term/sessions/<session_id>/` — Neuron's files for one
/// session.
const SESSIONS_DIRNAME: &str = "sessions";

/// `app_data/swarm-term/homes/` — pane HOMEs of builds before the shared
/// configuration. Each holds copies of the user's Claude credentials and
/// settings; the sweep deletes them unread.
const LEGACY_HOMES_DIRNAME: &str = "homes";

/// Agent id → claude session id, written before any pane spawns so a
/// session that never reaches `stop()` still names the transcripts the
/// next start must delete.
const MANIFEST_FILENAME: &str = "panes.json";

/// `app_data/swarm-term/`.
pub(crate) fn swarm_term_data_dir(app_data: &Path) -> PathBuf {
    app_data.join("swarm-term")
}

/// `app_data/swarm-term/sessions/<session_id>/`.
pub(crate) fn session_dir(app_data: &Path, session_id: &str) -> PathBuf {
    swarm_term_data_dir(app_data).join(SESSIONS_DIRNAME).join(session_id)
}

/// The persona + bridge-protocol prompt file of one agent (passed with
/// `--append-system-prompt-file`).
pub(crate) fn persona_file(session_dir: &Path, agent_id: &str) -> PathBuf {
    session_dir.join(format!("{agent_id}.persona.md"))
}

/// A fresh random (version 4) UUID for claude's `--session-id`.
pub(crate) fn new_claude_session_id() -> String {
    let mut b: [u8; 16] = rand::random();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// The claude arguments every pane gets besides its permission flags.
pub(crate) fn claude_args(claude_session: &str, persona_file: &Path) -> Vec<String> {
    vec![
        "--setting-sources".into(),
        "project".into(),
        "--strict-mcp-config".into(),
        "--session-id".into(),
        claude_session.into(),
        "--append-system-prompt-file".into(),
        persona_file.display().to_string(),
    ]
}

/// [`claude_args`] for a restarted pane: the same flags, but `--resume`
/// instead of `--session-id` — the agent's conversation goes on in the
/// same transcript.
pub(crate) fn claude_resume_args(claude_session: &str, persona_file: &Path) -> Vec<String> {
    let mut args = claude_args(claude_session, persona_file);
    if let Some(flag) = args.iter_mut().find(|a| *a == "--session-id") {
        *flag = "--resume".into();
    }
    args
}

/// The environment every pane gets on top of Neuron's own (which the
/// terminal registry has already scrubbed of a parent Claude session's
/// variables).
pub(crate) fn pane_env() -> HashMap<String, String> {
    [
        // A mid-session self-update exits the REPL and severs the PTY;
        // updates are user-triggered ("Update Claude") with no session
        // running. The flag was renamed between versions, so both.
        ("DISABLE_AUTOUPDATER", "1"),
        ("CLAUDE_CODE_DISABLE_AUTOUPDATE", "1"),
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
        // Present but empty: a token or key in the user's environment
        // would take precedence over their stored login.
        ("CLAUDE_CODE_OAUTH_TOKEN", ""),
        ("ANTHROPIC_API_KEY", ""),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// Where claude keeps transcripts: `<config dir>/projects`. The config
/// dir is `~/.claude` (the registry strips `CLAUDE_CONFIG_DIR`); claude
/// resolves `~` from `USERPROFILE` on Windows, so both spellings of home
/// are candidates.
pub(crate) fn claude_projects_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for var in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var).filter(|v| !v.is_empty()) {
            let dir = PathBuf::from(home).join(".claude").join("projects");
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// The transcript of claude session `claude_session`, if it exists yet.
/// Resolved `<session>.jsonl` paths, so the readiness waits and the
/// session monitor don't walk every folder under `~/.claude/projects`
/// on each poll (a heavy-claude user has hundreds). Keyed by the unique
/// claude session id; pruned by [`remove_transcripts`] when a session
/// stops.
static TRANSCRIPT_PATHS: std::sync::Mutex<std::collections::BTreeMap<String, PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

pub(crate) fn find_transcript(projects_dirs: &[PathBuf], claude_session: &str) -> Option<PathBuf> {
    if let Ok(cache) = TRANSCRIPT_PATHS.lock() {
        if let Some(path) = cache.get(claude_session) {
            if path.is_file() {
                return Some(path.clone());
            }
        }
    }
    let name = format!("{claude_session}.jsonl");
    let found = projects_dirs.iter().find_map(|projects| {
        std::fs::read_dir(projects)
            .ok()?
            .flatten()
            .map(|project| project.path().join(&name))
            .find(|file| file.is_file())
    });
    if let (Some(path), Ok(mut cache)) = (&found, TRANSCRIPT_PATHS.lock()) {
        cache.insert(claude_session.to_string(), path.clone());
    }
    found
}

/// Delete the transcripts (and their per-session sidecar directories) of
/// the given claude sessions. Returns the paths that could not be
/// removed yet (a claude process still exiting holds them open).
pub(crate) fn remove_transcripts<'a>(
    projects_dirs: &[PathBuf],
    claude_sessions: impl IntoIterator<Item = &'a str> + Clone,
) -> Vec<PathBuf> {
    if let Ok(mut cache) = TRANSCRIPT_PATHS.lock() {
        for id in claude_sessions.clone() {
            cache.remove(id);
        }
    }
    let mut left = Vec::new();
    for projects in projects_dirs {
        let Ok(entries) = std::fs::read_dir(projects) else {
            continue;
        };
        for project in entries.flatten().map(|e| e.path()) {
            for id in claude_sessions.clone() {
                let file = project.join(format!("{id}.jsonl"));
                if file.is_file() && std::fs::remove_file(&file).is_err() {
                    left.push(file);
                }
                let sidecar = project.join(id);
                if sidecar.is_dir() && std::fs::remove_dir_all(&sidecar).is_err() {
                    left.push(sidecar);
                }
            }
        }
    }
    left
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PaneManifest {
    /// Agent id → the `--session-id` its claude runs with.
    pub claude_sessions: BTreeMap<String, String>,
}

impl PaneManifest {
    pub(crate) fn write(&self, session_dir: &Path) -> Result<(), AppError> {
        let path = session_dir.join(MANIFEST_FILENAME);
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Internal(format!("pane manifest: {e}")))?;
        std::fs::write(&path, json)
            .map_err(|e| AppError::Internal(format!("write {}: {e}", path.display())))
    }

    /// `None` when the directory has no (readable) manifest.
    pub(crate) fn read(session_dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(session_dir.join(MANIFEST_FILENAME)).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// Remove a stopped session's files: its transcripts, then its Neuron
/// session directory. Returns what is left (to retry).
pub(crate) fn remove_session_files(projects_dirs: &[PathBuf], session_dir: &Path) -> Vec<PathBuf> {
    let mut left = Vec::new();
    if let Some(manifest) = PaneManifest::read(session_dir) {
        left = remove_transcripts(projects_dirs, manifest.claude_sessions.values().map(String::as_str));
    }
    if left.is_empty() {
        match std::fs::remove_dir_all(session_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => left.push(session_dir.to_path_buf()),
        }
    }
    left
}

/// What [`sweep_orphans`] removed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Swept {
    pub sessions: usize,
    pub legacy_homes: usize,
}

/// Delete what sessions that never ran `stop()` (app killed, crashed) left
/// behind: their transcripts and session directories, and every pane-HOME
/// tree of older builds (credential and settings COPIES — deleted without
/// reading them). Trees still beating belong to a session of another
/// Neuron instance sharing this app-data dir and are skipped (see
/// [`heartbeat`]). Call only when this process runs no session.
pub(crate) fn sweep_orphans(swarm_term_dir: &Path, projects_dirs: &[PathBuf]) -> Swept {
    let mut swept = Swept::default();
    for (dirname, legacy) in [(SESSIONS_DIRNAME, false), (LEGACY_HOMES_DIRNAME, true)] {
        let Ok(entries) = std::fs::read_dir(swarm_term_dir.join(dirname)) else {
            continue;
        };
        for tree in entries.flatten().map(|e| e.path()) {
            if !tree.is_dir() || heartbeat::is_live(&tree) {
                continue;
            }
            let left = if legacy {
                std::fs::remove_dir_all(&tree).err().map(|_| vec![tree.clone()]).unwrap_or_default()
            } else {
                remove_session_files(projects_dirs, &tree)
            };
            if left.is_empty() {
                if legacy {
                    swept.legacy_homes += 1;
                } else {
                    swept.sessions += 1;
                }
            } else {
                tracing::warn!(
                    path = %tree.display(),
                    left = left.len(),
                    "swarm-term: orphan cleanup incomplete; retried at the next start"
                );
            }
        }
    }
    swept
}

/// Resolve the user's real home directory: `HOME` first, then
/// `USERPROFILE` on Windows (no `dirs` dependency for one call site).
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

    #[test]
    fn claude_session_ids_are_distinct_version_4_uuids() {
        let a = new_claude_session_id();
        let b = new_claude_session_id();
        assert_ne!(a, b);
        for id in [&a, &b] {
            let groups: Vec<&str> = id.split('-').collect();
            assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12], "{id}");
            assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()), "{id}");
            assert!(groups[2].starts_with('4'), "version nibble: {id}");
            assert!(matches!(groups[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'), "variant: {id}");
        }
    }

    #[test]
    fn panes_keep_the_users_settings_mcp_servers_and_memory_out() {
        let args = claude_args("11111111-2222-4333-8444-555555555555", Path::new("p.md"));
        let pair = |flag: &str| {
            args.iter().position(|a| a == flag).map(|i| args.get(i + 1).cloned())
        };
        assert_eq!(pair("--setting-sources"), Some(Some("project".into())));
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        assert_eq!(
            pair("--session-id"),
            Some(Some("11111111-2222-4333-8444-555555555555".into()))
        );
        assert_eq!(pair("--append-system-prompt-file"), Some(Some("p.md".into())));
        let env = pane_env();
        assert_eq!(env.get("CLAUDE_CODE_DISABLE_AUTO_MEMORY").map(String::as_str), Some("1"));
        assert_eq!(env.get("CLAUDE_CODE_OAUTH_TOKEN").map(String::as_str), Some(""));
        assert_eq!(env.get("ANTHROPIC_API_KEY").map(String::as_str), Some(""));
        // The shared configuration: nothing points claude at another home.
        for var in ["HOME", "USERPROFILE", "CLAUDE_CONFIG_DIR"] {
            assert!(!env.contains_key(var), "{var}");
        }
    }

    #[test]
    fn a_restarted_pane_resumes_the_same_conversation() {
        let id = "11111111-2222-4333-8444-555555555555";
        let fresh = claude_args(id, Path::new("p.md"));
        let resumed = claude_resume_args(id, Path::new("p.md"));
        assert!(!resumed.iter().any(|a| a == "--session-id"));
        let at = resumed.iter().position(|a| a == "--resume").expect("--resume");
        assert_eq!(resumed[at + 1], id);
        // Every other flag is the fresh pane's.
        let rest = |args: &[String]| {
            args.iter().filter(|a| *a != "--resume" && *a != "--session-id").cloned().collect::<Vec<_>>()
        };
        assert_eq!(rest(&resumed), rest(&fresh));
    }

    #[test]
    fn transcripts_are_found_and_removed_by_claude_session_id_only() {
        let base = tempfile::tempdir().expect("tempdir");
        let projects = base.path().join("projects");
        let mine = projects.join("C--proj");
        let other = projects.join("C--elsewhere");
        std::fs::create_dir_all(mine.join("aaaa")).unwrap(); // sidecar dir of session aaaa
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(mine.join("aaaa.jsonl"), "{}").unwrap();
        std::fs::write(mine.join("users-own.jsonl"), "{}").unwrap();
        std::fs::write(other.join("bbbb.jsonl"), "{}").unwrap();
        let dirs = vec![projects.clone(), base.path().join("missing")];

        assert_eq!(find_transcript(&dirs, "aaaa"), Some(mine.join("aaaa.jsonl")));
        assert_eq!(find_transcript(&dirs, "bbbb"), Some(other.join("bbbb.jsonl")));
        assert_eq!(find_transcript(&dirs, "cccc"), None);

        assert!(remove_transcripts(&dirs, ["aaaa", "bbbb"]).is_empty());
        assert!(!mine.join("aaaa.jsonl").exists() && !mine.join("aaaa").exists());
        assert!(!other.join("bbbb.jsonl").exists());
        // The user's own session in the same project is untouched.
        assert!(mine.join("users-own.jsonl").exists());
    }

    #[test]
    fn a_session_is_removed_with_exactly_its_transcripts() {
        let base = tempfile::tempdir().expect("tempdir");
        let projects = base.path().join("projects").join("C--proj");
        std::fs::create_dir_all(&projects).unwrap();
        let session = session_dir(base.path(), "swarm-term-01X");
        std::fs::create_dir_all(&session).unwrap();
        let manifest = PaneManifest {
            claude_sessions: BTreeMap::from([
                ("orchestrator".to_string(), "aaaa".to_string()),
                ("scout".to_string(), "bbbb".to_string()),
            ]),
        };
        manifest.write(&session).unwrap();
        assert_eq!(PaneManifest::read(&session), Some(manifest));
        for name in ["aaaa.jsonl", "bbbb.jsonl", "keep.jsonl"] {
            std::fs::write(projects.join(name), "{}").unwrap();
        }
        let dirs = vec![base.path().join("projects")];
        assert!(remove_session_files(&dirs, &session).is_empty());
        assert!(!session.exists());
        assert!(!projects.join("aaaa.jsonl").exists() && !projects.join("bbbb.jsonl").exists());
        assert!(projects.join("keep.jsonl").exists());
    }

    #[test]
    fn the_sweep_removes_dead_sessions_and_legacy_homes_but_not_live_ones() {
        let base = tempfile::tempdir().expect("tempdir");
        let swarm_dir = swarm_term_data_dir(base.path());
        let projects = base.path().join("projects").join("C--proj");
        std::fs::create_dir_all(&projects).unwrap();
        let dirs = vec![base.path().join("projects")];

        let dead = session_dir(base.path(), "swarm-term-DEAD");
        std::fs::create_dir_all(&dead).unwrap();
        PaneManifest { claude_sessions: BTreeMap::from([("scout".into(), "dead1".into())]) }
            .write(&dead)
            .unwrap();
        std::fs::write(projects.join("dead1.jsonl"), "{}").unwrap();

        let live = session_dir(base.path(), "swarm-term-LIVE");
        std::fs::create_dir_all(&live).unwrap();
        PaneManifest { claude_sessions: BTreeMap::from([("scout".into(), "live1".into())]) }
            .write(&live)
            .unwrap();
        std::fs::write(projects.join("live1.jsonl"), "{}").unwrap();
        heartbeat::beat(&live);

        let legacy = swarm_dir.join(LEGACY_HOMES_DIRNAME).join("swarm-term-OLD").join("scout").join(".claude");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join(".credentials.json"), "copy").unwrap();
        let legacy_live = swarm_dir.join(LEGACY_HOMES_DIRNAME).join("swarm-term-OTHER");
        std::fs::create_dir_all(&legacy_live).unwrap();
        heartbeat::beat(&legacy_live);

        let swept = sweep_orphans(&swarm_dir, &dirs);
        assert_eq!(swept, Swept { sessions: 1, legacy_homes: 1 });
        assert!(!dead.exists() && !projects.join("dead1.jsonl").exists());
        assert!(live.exists() && projects.join("live1.jsonl").exists());
        assert!(!swarm_dir.join(LEGACY_HOMES_DIRNAME).join("swarm-term-OLD").exists());
        assert!(legacy_live.exists());
        // Nothing left to sweep.
        assert_eq!(sweep_orphans(&swarm_dir, &dirs), Swept::default());
    }
}

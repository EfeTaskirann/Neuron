//! Checks before the nine panes open (C2, K11): is the `claude` CLI new
//! enough for the flags the panes use, and is the user logged in to it?
//! Without them an old CLI or a missing login shows up as nine broken
//! panes — or nine login screens — instead of one clear message.
//!
//! Both checks run claude with exactly the environment a pane gets, so
//! they see the login a pane would see.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use crate::error::AppError;
use crate::swarm::binding::ClaudeSpawn;
use crate::swarm_term::pane_config;

/// The oldest claude this build runs: the pane flags
/// (`--setting-sources`, `--strict-mcp-config`, `--session-id`,
/// `dontAsk`, the deny-rule syntax) and the trust / readiness handling
/// were verified live against it.
pub(crate) const MIN_CLAUDE_VERSION: Version = Version(2, 1, 296);

/// The newest claude this build was verified against. A newer one runs,
/// with a note — a single claude release has broken every pane before.
pub(crate) const TESTED_CLAUDE_VERSION: Version = Version(2, 1, 296);

/// How long one check may take: the first claude start after boot can
/// spend tens of seconds in an antivirus scan (SETUP.md, pitfall 3).
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Version(pub u32, pub u32, pub u32);

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// Pure: the version in `claude --version` output (`2.1.296 (Claude Code)`).
pub(crate) fn parse_version(output: &str) -> Option<Version> {
    let mut parts = output.split_whitespace().next()?.split('.');
    let mut next = || parts.next()?.parse::<u32>().ok();
    let version = Version(next()?, next()?, next()?);
    next().is_none().then_some(version)
}

/// The fields of `claude auth status` (JSON) the checks use. The
/// output also names the account (email, organisation); those are
/// deliberately not read.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    logged_in: bool,
    #[serde(default)]
    projects_directory: Option<PathBuf>,
}

/// What the checks found. A too-old CLI or a missing login is an error
/// instead.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Preflight {
    pub version: Option<Version>,
    /// Where claude keeps transcripts, as `claude auth status` says.
    pub projects_dir: Option<PathBuf>,
    /// Shown to the user; does not block the start.
    pub note: Option<String>,
}

/// Pure: the verdict. `version_out` is the stdout of a successful
/// `claude --version`; `auth` the exit status and stdout of
/// `claude auth status` — `None` for a check that could not run.
/// A check that could not run or be read is skipped with a note: only
/// a definite answer (an old version, `loggedIn: false`) blocks.
pub(crate) fn judge(version_out: Option<&str>, auth: Option<(bool, &str)>) -> Result<Preflight, AppError> {
    let mut notes: Vec<String> = Vec::new();
    let version = version_out.and_then(parse_version);
    match version {
        Some(v) if v < MIN_CLAUDE_VERSION => {
            return Err(AppError::ClaudeVersionUnsupported(format!(
                "claude {v} kurulu; Neuron en az {MIN_CLAUDE_VERSION} istiyor. \
                 Sağ alttaki \"Update Claude\" ile güncelle."
            )));
        }
        Some(v) if v > TESTED_CLAUDE_VERSION => notes.push(format!(
            "claude {v} bu Neuron sürümüyle test edilmedi (test edilen: \
             {TESTED_CLAUDE_VERSION}); pane'lerde beklenmedik bir şey görürsen \
             sebebi sürüm olabilir."
        )),
        Some(_) => {}
        None => notes.push("claude sürümü okunamadı; sürüm kontrolü atlandı.".into()),
    }
    let mut projects_dir = None;
    // Documented: exit 0 when logged in, 1 when not, JSON either way.
    match auth.map(|(ok, out)| (ok, serde_json::from_str::<AuthStatus>(out.trim()).ok())) {
        Some((_, Some(status))) if !status.logged_in => {
            return Err(AppError::ClaudeLoginRequired(
                "Bir terminalde `claude` çalıştırıp `/login` ile giriş yap, sonra \
                 swarm'ı yeniden başlat."
                    .into(),
            ));
        }
        Some((_, Some(status))) => projects_dir = status.projects_directory,
        Some((true, None)) => {}
        Some((false, None)) | None => {
            notes.push("claude giriş durumu okunamadı; giriş kontrolü atlandı.".into())
        }
    }
    Ok(Preflight {
        version,
        projects_dir,
        note: (!notes.is_empty()).then(|| notes.join(" ")),
    })
}

/// Run both checks (in parallel) and judge them.
pub(crate) async fn run(spawn: &ClaudeSpawn) -> Result<Preflight, AppError> {
    let (version, auth) = tokio::join!(capture(spawn, &["--version"]), capture(spawn, &["auth", "status"]));
    let version_out = version.as_ref().filter(|(ok, _)| *ok).map(|(_, out)| out.as_str());
    judge(version_out, auth.as_ref().map(|(ok, out)| (*ok, out.as_str())))
}

/// Run claude with `args` as a pane would (same environment). `None`
/// when it could not run or ran past [`CHECK_TIMEOUT`].
async fn capture(spawn: &ClaudeSpawn, args: &[&str]) -> Option<(bool, String)> {
    let mut cmd = Command::new(&spawn.program);
    cmd.args(&spawn.prefix_args)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for var in crate::sidecar::terminal::claude_child_env_removals() {
        cmd.env_remove(var);
    }
    cmd.envs(pane_config::pane_env());
    // No console window flashing up from the GUI app.
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let output = tokio::time::timeout(CHECK_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    Some((output.status.success(), String::from_utf8_lossy(&output.stdout).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGGED_IN: &str = r#"{"loggedIn": true, "authMethod": "claude.ai",
        "projectsDirectory": "C:\\Users\\x\\.claude\\projects", "email": "x@example.com"}"#;
    const LOGGED_OUT: &str = r#"{"loggedIn": false, "authMethod": "none"}"#;

    #[test]
    fn versions_are_read_from_the_cli_banner() {
        assert_eq!(parse_version("2.1.296 (Claude Code)\n"), Some(Version(2, 1, 296)));
        assert_eq!(parse_version("10.0.1"), Some(Version(10, 0, 1)));
        for garbage in ["", "Claude Code", "2.1", "2.1.x (Claude Code)", "2.1.296.4", "v2.1.296"] {
            assert_eq!(parse_version(garbage), None, "{garbage:?}");
        }
        assert!(Version(2, 1, 295) < MIN_CLAUDE_VERSION && MIN_CLAUDE_VERSION <= TESTED_CLAUDE_VERSION);
        assert_eq!(Version(2, 1, 296).to_string(), "2.1.296");
    }

    #[test]
    fn a_supported_logged_in_cli_passes_with_its_transcript_root() {
        let p = judge(Some("2.1.296 (Claude Code)"), Some((true, LOGGED_IN))).unwrap();
        assert_eq!(p.version, Some(Version(2, 1, 296)));
        assert_eq!(p.projects_dir, Some(PathBuf::from(r"C:\Users\x\.claude\projects")));
        assert_eq!(p.note, None);
    }

    #[test]
    fn an_old_cli_is_refused_with_how_to_update() {
        let err = judge(Some("2.1.250 (Claude Code)"), Some((true, LOGGED_IN))).unwrap_err();
        assert_eq!(err.kind(), "claude_version_unsupported");
        assert!(err.message().contains("2.1.250") && err.message().contains("Update Claude"));
    }

    #[test]
    fn a_missing_login_is_refused_with_how_to_log_in() {
        // `claude auth status` exits 1 and still prints JSON when logged out.
        let err = judge(Some("2.1.296 (Claude Code)"), Some((false, LOGGED_OUT))).unwrap_err();
        assert_eq!(err.kind(), "claude_login_required");
        assert!(err.message().contains("/login"));
        // A JSON "not logged in" counts even with exit 0.
        assert!(judge(Some("2.1.296"), Some((true, LOGGED_OUT))).is_err());
    }

    #[test]
    fn a_newer_cli_runs_with_a_note() {
        let p = judge(Some("2.1.300 (Claude Code)"), Some((true, LOGGED_IN))).unwrap();
        assert!(p.note.as_deref().is_some_and(|n| n.contains("2.1.300") && n.contains("test edilmedi")));
    }

    /// Opt-in: both checks against the installed claude, with the pane
    /// environment (this host's own Claude session variables stripped).
    /// Spends no tokens.
    ///
    ///   cargo test --lib swarm_term::preflight::tests::live -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "runs the installed claude CLI — opt in"]
    async fn live_preflight_against_the_installed_claude() {
        let spawn = crate::swarm::binding::resolve_claude_spawn().expect("claude CLI");
        let p = run(&spawn).await.expect("a logged-in, supported claude passes");
        println!("version {:?}, projects dir {:?}, note {:?}", p.version, p.projects_dir, p.note);
        assert!(p.version.is_some_and(|v| v >= MIN_CLAUDE_VERSION));
        assert!(p.projects_dir.is_some_and(|d| d.is_dir()));
    }

    #[test]
    fn a_check_that_cannot_run_or_be_read_is_skipped_not_fatal() {
        let p = judge(None, None).unwrap();
        assert_eq!(p.version, None);
        let note = p.note.unwrap();
        assert!(note.contains("sürümü okunamadı") && note.contains("giriş durumu okunamadı"));
        // Exit 0 without readable JSON: logged in, nothing to say.
        assert_eq!(judge(Some("2.1.296"), Some((true, "garbage"))).unwrap().note, None);
        // Exit 1 without JSON (e.g. an unknown subcommand): skipped.
        assert!(judge(Some("2.1.296"), Some((false, "error: unknown command 'auth'"))).unwrap().note.is_some());
    }
}

//! Checks before the nine panes open (C2, K11): is the `claude` CLI new
//! enough for the flags the panes use, and is the user logged in to it?
//! Without them an old CLI or a missing login shows up as nine broken
//! panes — or nine login screens — instead of one clear message.
//!
//! Both checks run claude with exactly the environment a pane gets, so
//! they see the login a pane would see.
//!
//! A third check probes the PROJECT: its cheap verification commands
//! (a typecheck, `cargo check`, a lint — never the tests) run once
//! before the panes open. A missing toolchain (`'tsc' is not
//! recognized`) is one clear error naming the install command, instead
//! of nine tasks failing on it later (the live EvE run). A check that
//! runs but fails is the project's state, not a setup gap: the start
//! goes on and the agents are told it was broken before they began.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use crate::error::AppError;
use crate::swarm::binding::ClaudeSpawn;
use crate::swarm_term::pane_config;
use crate::swarm_term::project_facts::ProjectFacts;

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

// ── the environment probe ─────────────────────────────────────────── //

/// How long one probe command may run: a cold `cargo check` on a large
/// crate. Past it the probe says nothing (`Unknown`), never "broken".
const PROBE_TIMEOUT: Duration = Duration::from_secs(120);

/// The commands worth probing: they answer in seconds and change
/// nothing. Tests are left out (minutes, side effects).
const PROBE_MARKS: &[&str] = &["typecheck", "type-check", "tsc", "cargo check", "lint", "go vet"];

/// How a shell says the tool is not there — cmd.exe, bash, node, and
/// the exit codes they use for it (9009 cmd.exe, 127 POSIX shells).
const MISSING_MARKS: &[&str] = &[
    "is not recognized as an internal or external command",
    "is not recognized",
    "command not found",
    "cannot find module",
    "no such file or directory",
    "enoent",
];

/// What one probe command came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProbeVerdict {
    Ok,
    /// The tool ran and reported problems: the project's own state.
    Broken(i32),
    /// The tool is not installed: a setup gap the user must close.
    Missing,
    /// Timed out or could not be started: nothing is known.
    Unknown,
}

/// What the probe found when nothing blocks the start.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct EnvProbe {
    /// `<command> (exit <code>)` for every check that ran and failed.
    pub broken: Vec<String>,
}

/// Pure: the project commands the probe runs, in their listed order.
pub(crate) fn probe_commands(facts: &ProjectFacts) -> Vec<String> {
    facts
        .commands
        .iter()
        .chain(&facts.exact_commands)
        .filter(|c| PROBE_MARKS.iter().any(|m| c.contains(m)))
        .cloned()
        .collect()
}

/// Pure: a probe's verdict from its exit code and combined output.
pub(crate) fn classify(code: Option<i32>, output: &str) -> ProbeVerdict {
    let lower = output.to_lowercase();
    match code {
        None => ProbeVerdict::Unknown,
        Some(0) => ProbeVerdict::Ok,
        Some(127) | Some(9009) => ProbeVerdict::Missing,
        Some(_) if MISSING_MARKS.iter().any(|m| lower.contains(m)) => ProbeVerdict::Missing,
        Some(c) => ProbeVerdict::Broken(c),
    }
}

/// Pure: the start verdict over `(command, verdict, output tail)`
/// results. A missing tool refuses the start and names `installs` (the
/// project's install commands) as the way out; failing checks are
/// returned for the personas.
pub(crate) fn judge_environment(
    results: &[(String, ProbeVerdict, String)],
    installs: &[String],
) -> Result<EnvProbe, AppError> {
    if let Some((cmd, _, said)) = results.iter().find(|(_, v, _)| *v == ProbeVerdict::Missing) {
        let fix = if installs.is_empty() {
            "Önce projenin bağımlılıklarını kur (README'sine bak)".to_string()
        } else {
            format!(
                "Önce bağımlılıkları kur: {}",
                installs.iter().map(|i| format!("`{i}`")).collect::<Vec<_>>().join(", ")
            )
        };
        let said = said.trim();
        let said = if said.is_empty() { String::new() } else { format!(" — {said}") };
        return Err(AppError::EnvironmentNotReady(format!(
            "Proje ortamı hazır değil: `{cmd}` çalışmadı{said}. {fix}, sonra swarm'ı \
             yeniden başlat."
        )));
    }
    Ok(EnvProbe {
        broken: results
            .iter()
            .filter_map(|(cmd, v, _)| match v {
                ProbeVerdict::Broken(code) => Some(format!("{cmd} (exit {code})")),
                _ => None,
            })
            .collect(),
    })
}

/// Run the project's cheap checks in `project`, one after the other, and
/// judge them. Nothing to probe → an empty, passing result.
pub(crate) async fn probe_environment(project: &Path, facts: &ProjectFacts) -> Result<EnvProbe, AppError> {
    let mut results = Vec::new();
    for command in probe_commands(facts) {
        let (code, output) = run_probe(project, &command).await;
        let verdict = classify(code, &output);
        tracing::info!(command = %command, code = ?code, verdict = ?verdict, "swarm-term pre-flight: environment probe");
        results.push((command, verdict, tail(&output)));
    }
    judge_environment(&results, &facts.install_commands)
}

/// One probe through the OS shell (the commands are shell lines:
/// `npm --prefix web run typecheck`). `None` = timed out or the shell
/// itself could not start.
async fn run_probe(project: &Path, command: &str) -> (Option<i32>, String) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    cmd.current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(out)) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            (out.status.code(), text)
        }
        Ok(Err(e)) => (None, e.to_string()),
        Err(_) => (None, "timed out".into()),
    }
}

/// The last line or so of a probe's output, on one line, for the error
/// message.
fn tail(output: &str) -> String {
    let flat: String = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let start = flat.char_indices().rev().nth(199).map_or(0, |(i, _)| i);
    flat[start..].to_string()
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

    // -- the environment probe ---------------------------------------- //

    fn facts(commands: &[&str], exact: &[&str], installs: &[&str]) -> ProjectFacts {
        ProjectFacts {
            commands: commands.iter().map(|s| s.to_string()).collect(),
            exact_commands: exact.iter().map(|s| s.to_string()).collect(),
            install_commands: installs.iter().map(|s| s.to_string()).collect(),
            ..ProjectFacts::default()
        }
    }

    #[test]
    fn the_probe_runs_the_cheap_checks_not_the_tests() {
        // Tests can take minutes and have side effects; a typecheck or a
        // `cargo check` says in seconds whether the toolchain is there.
        let f = facts(
            &["cargo check", "cargo test", "npm --prefix web run typecheck", "npm --prefix web test", "pnpm lint", "pytest"],
            &["node api/node_modules/typescript/bin/tsc -p api --noEmit"],
            &[],
        );
        assert_eq!(
            probe_commands(&f),
            [
                "cargo check",
                "npm --prefix web run typecheck",
                "pnpm lint",
                "node api/node_modules/typescript/bin/tsc -p api --noEmit",
            ]
        );
        assert!(probe_commands(&ProjectFacts::default()).is_empty());
    }

    #[test]
    fn classify_tells_a_missing_tool_from_a_failing_check() {
        // cmd.exe, bash and node each say "no such tool" their own way.
        for (code, out) in [
            (Some(1), "'tsc' is not recognized as an internal or external command,\noperable program or batch file."),
            (Some(127), "bash: vitest: command not found"),
            (Some(1), "Error: Cannot find module 'C:\\x\\node_modules\\typescript\\bin\\tsc'"),
            (Some(9009), ""),
        ] {
            assert_eq!(classify(code, out), ProbeVerdict::Missing, "{code:?} {out:?}");
        }
        // The tool ran and found real problems: that is the project's state, not a setup gap.
        assert_eq!(classify(Some(2), "src/account.ts(247,22): error TS7006: Parameter 'order' implicitly has an 'any' type."), ProbeVerdict::Broken(2));
        assert_eq!(classify(Some(101), "error[E0425]: cannot find value `x`"), ProbeVerdict::Broken(101));
        assert_eq!(classify(Some(0), "Finished `dev` profile"), ProbeVerdict::Ok);
        // Killed or timed out: nothing is known.
        assert_eq!(classify(None, ""), ProbeVerdict::Unknown);
    }

    #[test]
    fn a_missing_tool_blocks_the_start_and_names_the_install_command() {
        let results = vec![(
            "npm --prefix storefront run typecheck".to_string(),
            ProbeVerdict::Missing,
            "'tsc' is not recognized as an internal or external command".to_string(),
        )];
        let err = judge_environment(&results, &["npm --prefix storefront install".to_string()]).unwrap_err();
        assert_eq!(err.kind(), "environment_not_ready");
        let m = err.message();
        assert!(m.contains("npm --prefix storefront run typecheck"), "{m}");
        assert!(m.contains("'tsc' is not recognized"), "{m}");
        assert!(m.contains("npm --prefix storefront install"), "{m}");
    }

    #[test]
    fn a_failing_check_does_not_block_but_is_remembered() {
        let results = vec![
            ("cargo check".to_string(), ProbeVerdict::Broken(101), "error[E0425]".to_string()),
            ("pnpm lint".to_string(), ProbeVerdict::Ok, String::new()),
            ("pnpm typecheck".to_string(), ProbeVerdict::Unknown, String::new()),
        ];
        let probe = judge_environment(&results, &[]).unwrap();
        assert_eq!(probe.broken, ["cargo check (exit 101)"]);
        // Without an install command the message still says what to do.
        let results = vec![("cargo check".to_string(), ProbeVerdict::Missing, "'cargo' is not recognized".to_string())];
        let m = judge_environment(&results, &[]).unwrap_err().message().into_owned();
        assert!(m.contains("kur"), "{m}");
    }

    /// Opt-in: real shell runs of the probe (needs npm on PATH).
    ///
    ///   cargo test --lib swarm_term::preflight::tests::live_probe -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "runs npm through the OS shell — opt in"]
    async fn live_probe_of_a_missing_tool_blocks_and_a_failing_check_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        // A typecheck script whose tool does not exist: a setup gap.
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"p","scripts":{"typecheck":"neuron-definitely-missing-tool --noEmit"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        let facts = crate::swarm_term::project_facts::detect(dir.path());
        assert_eq!(facts.commands, ["npm run typecheck"]);
        let err = probe_environment(dir.path(), &facts).await.unwrap_err();
        assert_eq!(err.kind(), "environment_not_ready");
        assert!(err.message().contains("npm ci"), "{}", err.message());
        // The tool runs and fails: the project's state, remembered, not fatal.
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"p","scripts":{"typecheck":"node -e \"process.exit(2)\""}}"#,
        )
        .unwrap();
        let facts = crate::swarm_term::project_facts::detect(dir.path());
        let probe = probe_environment(dir.path(), &facts).await.unwrap();
        assert_eq!(probe.broken, ["npm run typecheck (exit 2)"]);
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

//! Per-role tool permissions for the nine swarm-term claude panes.
//!
//! Every pane used to run with `--permission-mode bypassPermissions`:
//! no prompts, but also full shell + file access as the user, so
//! instructions planted in anything an agent reads (a README, an issue,
//! a dependency's code) could turn into commands on the user's machine,
//! and any agent could read the user's real `~/.claude/.credentials.json`.
//!
//! Panes now run in `dontAsk` mode with a per-role allowlist: anything
//! not on the list is denied WITHOUT prompting, so the swarm stays
//! autonomous. Rules (verified against claude 2.1.x):
//!
//!   * `Read(./**)` — read the project only; also scopes `Grep`/`Glob`.
//!     Reading outside the project (incl. the user's credentials) is
//!     denied.
//!   * `Edit(./…/**)` — governs every file-writing tool (`Write`, `Edit`,
//!     `NotebookEdit`). Non-builders may only write their OWN bridge
//!     outbox; builders may write inside the project.
//!   * `Bash(<prefix>:*)` — only the listed verification commands;
//!     nobody gets an unrestricted shell. (claude still auto-allows its
//!     own read-only commands such as `git status`, scoped to the
//!     project — so the roles without a shell deny the tool outright.)
//!
//! A deny list ([`denied_tools`], `--disallowedTools`) wins over every
//! allow rule, including ones from settings files: nobody writes
//! `.git` / `.claude` / editor config, the bridge's bookkeeping or
//! another agent's outbox or the bridge's legacy `inbox/`, nobody uses
//! the web tools, nobody reads the usual secret files (`.env`, SSH /
//! cloud keys), and the git options — and two-path forms — that write or
//! read outside the project are refused. Live probes against claude
//! 2.1.296 (`permissions/live_tests.rs`) found `git diff --output=../x`
//! writing outside the project and a builder forging a message in
//! `outbox/coordinator/` before these rules.
//!
//! Known residual (Windows-only, K3): the shell allowlist exposes exactly
//! one command that can read an arbitrary path — `git diff <a> <b>`,
//! which git runs in no-index mode when a path is outside the work tree.
//! The deny rules below catch its out-of-project forms written with
//! forward slashes (`C:/…`, `~/…`, `../…`); a PowerShell invocation using
//! back-slashes cannot be matched by a glob rule (claude treats `\` as a
//! glob escape — see the no-backslash invariant in the tests), so that
//! form is the documented residual. The structural fix (run the agents'
//! git through a Neuron wrapper pinned to the project) is a follow-up.
//!
//! Residual risk, by design: running a project's tests / build executes
//! that project's code — see the bridge's trust model. Setting
//! [`PERMISSIONS_ENV`]`=bypass` restores the old unrestricted mode for
//! every pane (logged as a warning and reported to the UI through
//! [`SwarmTermPermissionMode`]).

use serde::{Deserialize, Serialize};
use specta::Type;

use super::hierarchy::AGENT_IDS;
use super::project_facts::ProjectFacts;

#[cfg(test)]
mod live_tests;

/// Opt-out: `NEURON_TERM_PERMISSIONS=bypass` → every pane runs with
/// `bypassPermissions` again.
pub(crate) const PERMISSIONS_ENV: &str = "NEURON_TERM_PERMISSIONS";

/// The permission mode a session's panes were spawned with. Captured
/// once at session start, so what the UI reports is exactly what the
/// panes run with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SwarmTermPermissionMode {
    /// `dontAsk` + the per-role allowlist ([`allowed_tools`]).
    Restricted,
    /// [`PERMISSIONS_ENV`]`=bypass`: every pane runs with
    /// `bypassPermissions` (unrestricted shell + file access).
    Bypass,
}

/// The mode a session started now gets, from [`PERMISSIONS_ENV`].
pub(crate) fn current_mode() -> SwarmTermPermissionMode {
    mode_from(std::env::var(PERMISSIONS_ENV).ok().as_deref())
}

/// Pure: [`PERMISSIONS_ENV`]'s value → mode. Only `bypass` (trimmed,
/// any case) opts out.
fn mode_from(value: Option<&str>) -> SwarmTermPermissionMode {
    if value.is_some_and(|v| v.trim().eq_ignore_ascii_case("bypass")) {
        SwarmTermPermissionMode::Bypass
    } else {
        SwarmTermPermissionMode::Restricted
    }
}

/// Coarse role used to pick an allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// orchestrator, coordinator, scout, planner — read + message only.
    Coordinate,
    /// backend/frontend reviewers + integration-tester — read, message,
    /// run verification commands.
    Verify,
    /// backend/frontend builders — read + write the project, run
    /// verification commands.
    Build,
}

pub(crate) fn role_of(agent_id: &str) -> Role {
    match agent_id {
        "backend-builder" | "frontend-builder" => Role::Build,
        "backend-reviewer" | "frontend-reviewer" | "integration-tester" => Role::Verify,
        _ => Role::Coordinate,
    }
}

/// Verification commands available to the Verify and Build roles
/// (prefix match) in every project. Covers the common toolchains a
/// swarm may meet; the commands the project's own manifests name (e.g.
/// a monorepo package's `pnpm -C app test`) are added per session —
/// see `project_facts`.
const VERIFY_COMMANDS: &[&str] = &[
    "cargo check",
    "cargo test",
    "cargo clippy",
    "cargo build",
    "cargo fmt --check",
    "pnpm test",
    "pnpm typecheck",
    "pnpm lint",
    "pnpm exec vitest run",
    "pnpm exec tsc --noEmit",
    "npm test",
    "npm run test",
    "npm run lint",
    "npm run typecheck",
    "npx tsc --noEmit",
    "pytest",
    "python -m pytest",
    "go test",
    "go vet",
    "git status",
    "git diff",
    "git log",
    "git show",
];

/// Project directories no pane may write, whatever its role: `.git` (a
/// planted `config` — `core.fsmonitor`, `diff.external` — or hook runs
/// on the next `git status` / commit, as the user), `.claude` (settings
/// and commands a later Claude session runs), `.vscode` / `.idea`
/// (editor tasks and run configurations). claude 2.1.296 already
/// refuses these writes on its own; the rules keep that true across
/// claude versions.
const PROTECTED_DIRS: &[&str] = &[".git", ".claude", ".vscode", ".idea"];

/// Options that turn an allowed read-only git command into a write or a
/// read outside the project: `--output` writes the result to any path,
/// `--no-index` diffs arbitrary files, `--ext-diff` runs the configured
/// external diff program.
const GIT_ESCAPES: &[(&str, &str)] = &[
    ("git diff", "--output"),
    ("git log", "--output"),
    ("git show", "--output"),
    ("git diff", "--no-index"),
    ("git diff", "--ext-diff"),
    ("git log", "--ext-diff"),
    ("git show", "--ext-diff"),
    // `git diff <a> <b>` switches to no-index on its own when a path is
    // outside the work tree, reading any file without `--no-index`. These
    // match an out-of-project path ARGUMENT by its prefix. Forward-slash
    // only, so they never collide with gitrevision syntax (`HEAD:file`,
    // `a..b`, `HEAD~1`) and never need a backslash (see the tests).
    ("git diff", ":/"),
    ("git diff", "~/"),
    ("git diff", "../"),
];

/// Options that turn an allowed verification command into a write
/// outside the project, a read of another project, or a run of an
/// arbitrary program — the same names recur across cargo, tsc, vitest,
/// eslint, pytest and go, so they are denied in EVERY shell command of
/// the Verify and Build roles (a leading `*`). A project command's own
/// `-C app` / `--prefix backend` is not in the list; the exact install
/// rules carry none of these either (see the tests). Entries with a
/// trailing space or `=` are the forms that take a value.
const OUTPUT_ESCAPES: &[&str] = &[
    // cargo / rustc / libtest: build elsewhere, build another project,
    // write the test log anywhere.
    "--target-dir",
    "--out-dir",
    "--artifact-dir",
    "--manifest-path",
    "--logfile",
    // tsc: emit elsewhere, or re-enable emitting under `--noEmit`.
    "--outDir",
    "--outFile",
    "--declarationDir",
    "--tsBuildInfoFile",
    "--noEmit false",
    "--noEmit=false",
    "--project ",
    "--project=",
    // vitest / eslint / reporters: report files, another root.
    "--outputFile",
    "--output-file",
    "--coverage.reportsDirectory",
    "--root ",
    "--root=",
    "--dir ",
    "--dir=",
    " -o ",
    "-o=",
    // pytest: report, temp and log files anywhere, another root.
    "--junitxml",
    "--junit-xml",
    "--basetemp",
    "--rootdir",
    "--cov-report",
    "--log-file",
    // go test / go vet: profiles and binaries anywhere, a program of
    // the agent's choice run as the test / tool, files replaced from a
    // JSON map.
    "-outputdir",
    "-coverprofile",
    "-cpuprofile",
    "-memprofile",
    "-blockprofile",
    "-mutexprofile",
    "-trace=",
    "-exec ",
    "-exec=",
    "-toolexec",
    "-vettool",
    "-overlay",
    // Any tool's config file: cargo's `build.target-dir` override, a
    // vitest / eslint config outside the project that runs its code.
    "--config",
];

/// Secret files no pane may READ, whatever its role. The panes run on the
/// shared Claude config with `--setting-sources project`, so the user's
/// own `permissions.deny` rules no longer constrain them; this baseline
/// keeps the common credential files out of the model even though the
/// project-wide `Read(./**)` allow covers them (deny beats allow). A
/// project that must read one can opt the whole session out with
/// [`PERMISSIONS_ENV`]`=bypass`. `.env.example` / `.env.sample` stay
/// readable (only the real-secret conventions are listed).
const SECRET_READS: &[&str] = &[
    "./.env",
    "./.env.local",
    "./**/.env",
    "./**/.env.local",
    "./**/.env.*.local",
    "./**/*.pem",
    "./**/id_rsa",
    "./**/id_ed25519",
    "./**/.ssh/**",
    "./**/.aws/credentials",
    "./**/.npmrc",
    "./secrets/**",
];

/// The `claude` CLI arguments that set `agent_id`'s permissions under
/// `mode`, for a session whose bridge root is `.bridgespace/<session_id>/`
/// under the pane's working directory (the project). `facts` name the
/// verification and install commands of the project's manifests.
pub(crate) fn claude_permission_args(
    mode: SwarmTermPermissionMode,
    agent_id: &str,
    session_id: &str,
    facts: &ProjectFacts,
) -> Vec<String> {
    if mode == SwarmTermPermissionMode::Bypass {
        return vec!["--permission-mode".into(), "bypassPermissions".into()];
    }
    let mut args = vec!["--permission-mode".into(), "dontAsk".into(), "--allowedTools".into()];
    args.extend(allowed_tools(agent_id, session_id, facts));
    args.push("--disallowedTools".into());
    args.extend(denied_tools(agent_id, session_id));
    args
}

/// The verification commands of a session: the generic ones, then the
/// project's own (without repeats).
fn verify_commands(project_commands: &[String]) -> Vec<&str> {
    let mut all: Vec<&str> = VERIFY_COMMANDS.to_vec();
    for c in project_commands {
        if !all.contains(&c.as_str()) {
            all.push(c);
        }
    }
    all
}

/// Pure: the deny list for one agent. A deny rule beats every allow rule
/// (claude's own and any settings file's), so these hold even where an
/// allow rule such as a builder's `Edit(./**)` covers the path.
pub(crate) fn denied_tools(agent_id: &str, session_id: &str) -> Vec<String> {
    let mut rules: Vec<String> = PROTECTED_DIRS
        .iter()
        .map(|dir| format!("Edit(./{dir}/**)"))
        .collect();
    // The bridge's own bookkeeping, and every OTHER agent's outbox: a
    // file there is delivered as that agent's message (the sender is the
    // directory), so writing one forges a message.
    rules.push("Edit(./.bridgespace/.gitignore)".into());
    rules.push("Edit(./.bridgespace/_archive/**)".into());
    // Neuron's memory of the project (see `experience`): it feeds the
    // next session's personas, so an agent that could edit it would
    // write tomorrow's instructions.
    rules.push("Edit(./.bridgespace/experience.md)".into());
    rules.push(format!("Edit(./.bridgespace/{session_id}/processed/**)"));
    rules.push(format!("Edit(./.bridgespace/{session_id}/rejected/**)"));
    // The legacy `inbox/` drop path: the watcher still scans it and pastes
    // a `[Neuron]` notice into the pane the file's own `from` field names,
    // so a writable inbox lets a pane trigger a notice addressed as any
    // agent. Nobody writes it (real messages go to the sender's outbox).
    rules.push(format!("Edit(./.bridgespace/{session_id}/inbox/**)"));
    for other in AGENT_IDS.iter().filter(|other| **other != agent_id) {
        rules.push(format!("Edit(./.bridgespace/{session_id}/outbox/{other}/**)"));
    }
    // No pane talks to the network.
    rules.push("WebFetch".into());
    rules.push("WebSearch".into());
    // The common secret files stay out of every pane (see `SECRET_READS`).
    for secret in SECRET_READS {
        rules.push(format!("Read({secret})"));
    }
    if role_of(agent_id) == Role::Coordinate {
        rules.push("Bash".into());
        rules.push("PowerShell".into());
    } else {
        for (cmd, opt) in GIT_ESCAPES {
            rules.push(format!("Bash({cmd} *{opt}*)"));
            rules.push(format!("PowerShell({cmd} *{opt}*)"));
        }
        for opt in OUTPUT_ESCAPES {
            rules.push(format!("Bash(*{opt}*)"));
            rules.push(format!("PowerShell(*{opt}*)"));
        }
    }
    rules
}

/// Pure: the allowlist for one agent.
pub(crate) fn allowed_tools(agent_id: &str, session_id: &str, facts: &ProjectFacts) -> Vec<String> {
    let mut rules = vec![
        "Read(./**)".to_string(),
        "TodoWrite".to_string(),
        format!("Edit(./.bridgespace/{session_id}/outbox/{agent_id}/**)"),
    ];
    let role = role_of(agent_id);
    if role == Role::Build {
        rules.push("Edit(./**)".to_string());
        // Dependency installs (owner decisions: `npm ci`, then a bare
        // `npm install`): exact rules, with no room for arguments —
        // `npm install <package>` would install a package an injected
        // instruction picked, and `npm ci --prefix ../x` would install
        // outside the project. Residual risk, as for tests: the install
        // runs the dependencies' own install scripts.
        for c in &facts.install_commands {
            rules.push(format!("Bash({c})"));
            rules.push(format!("PowerShell({c})"));
        }
    }
    if matches!(role, Role::Verify | Role::Build) {
        // claude has a separate PowerShell tool on Windows; the same
        // commands must be reachable through it, with the same limits.
        for c in verify_commands(&facts.commands) {
            rules.push(format!("Bash({c}:*)"));
            rules.push(format!("PowerShell({c})"));
            rules.push(format!("PowerShell({c} *)"));
        }
        // Exact, with no room for arguments (see
        // `ProjectFacts::exact_commands`).
        for c in &facts.exact_commands {
            rules.push(format!("Bash({c})"));
            rules.push(format!("PowerShell({c})"));
        }
    }
    rules
}

/// Shell commands start in the project root (every one of them — see
/// `CLAUDE_BASH_MAINTAIN_PROJECT_WORKING_DIR` in `pane_config`); a `cd`
/// OUT of the project (live run: the tester `cd`-ed to the parent
/// repository) gets the whole command denied. A `cd` into a project
/// subdirectory is allowed and sometimes needed (a package without a
/// `-C` variant).
const NO_CD: &str = "Her shell komutu proje kökünde başlar; bir önceki komuttaki `cd` \
                     sonrakine taşınmaz. Komutu doğrudan çalıştır (ör. `cargo test`). Proje \
                     kökünün DIŞINA `cd` YAPMA — o komut reddedilir. Proje içindeki bir alt \
                     klasör gerekiyorsa `-C <klasör>` / `--prefix <klasör>` biçimini ya da \
                     aynı komutta `cd <alt-klasör> && <komut>` kullan.";

/// Human-readable summary for the persona footer, so an agent knows its
/// limits up front instead of discovering them through denied calls.
pub(crate) fn describe_for_persona(agent_id: &str, facts: &ProjectFacts) -> String {
    let commands = verify_commands(&facts.commands).join(", ");
    let installs = facts.install_commands.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
    // Who installs missing dependencies: a builder when the project has
    // an install command it may run, the user otherwise.
    let missing_deps = if installs.is_empty() {
        "Bağımlılıklar kurulu değilse (`node_modules` yok, `tsc` / `vitest` bulunamıyor) \
         kurmaya ÇALIŞMA: coordinator'a `hata — bağımlılıklar kurulu değil: <klasör>` yaz; \
         bu projede ajanların çalıştırabileceği bir kurulum komutu olmadığı için kurulum \
         kullanıcının işi."
            .to_string()
    } else {
        format!(
            "Bağımlılıklar kurulu değilse (`node_modules` yok, `tsc` / `vitest` bulunamıyor) \
             kurmaya ÇALIŞMA: coordinator'a `hata — bağımlılıklar kurulu değil: <klasör>` yaz; \
             builder {installs} ile kurar."
        )
    };
    // Commands that admit no argument are announced as such, so an agent
    // doesn't learn it from a denial.
    let exact = if facts.exact_commands.is_empty() {
        String::new()
    } else {
        let list = facts.exact_commands.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ");
        format!(" Şunlar da çalışır, ama yalnızca AYNEN (ek argüman olmadan): {list}.")
    };
    let base = "Proje dosyalarını OKUYABİLİRSİN (yalnızca proje klasörü içinde; \
                dışarısı reddedilir). Gizli dosyalar (`.env`, SSH/bulut anahtarları, \
                `.npmrc`) okunamaz. Mesajlarını yalnızca KENDİ outbox'una yazabilirsin; \
                başka bir ajanın outbox'u, `.git/`, `.claude/`, `.vscode/` ve `.idea/` \
                reddedilir. Web araçları kapalı.";
    let git = "`git diff` / `git log` / `git show` içinde `--output`, `--no-index` ve \
               `--ext-diff` seçenekleri reddedilir; `git diff` ile proje dışı bir yolu \
               (`C:/…`, `~/…`, `../…`) okumak da reddedilir. Doğrulama komutlarında çıktı \
               yolu, profil, dış proje ve program seçenekleri de reddedilir (`--target-dir`, \
               `--manifest-path`, `--outDir`, `--outputFile`, `--junitxml`, `-o`, \
               `-coverprofile`, `-exec`, `--config`, `--dir`, `--root` …): komutu çıktısını \
               proje dışına yazacak ya da başka bir projeyi okuyacak biçimde çağırma.";
    match role_of(agent_id) {
        Role::Coordinate => {
            let mut text = format!(
                "{base} Proje dosyalarını değiştiremez, shell (`Bash`) komutu ve web \
                 aracı kullanamazsın. Değişiklik gerekiyorsa bir builder'a dispatch et."
            );
            if !installs.is_empty() {
                text.push_str(&format!(
                    " Bir ajan bağımlılıkların kurulu olmadığını bildirirse kurulumu \
                     builder'a dispatch et ({installs})."
                ));
            }
            text
        }
        Role::Verify => format!(
            "{base} Proje dosyalarını değiştiremezsin. `Bash` ile yalnızca doğrulama \
             komutlarını çalıştırabilirsin: {}.{exact} {git} {NO_CD} {missing_deps}",
            commands
        ),
        Role::Build => {
            let install = if installs.is_empty() {
                "Paket kurmak / başka bir komut gerekiyorsa coordinator'a `hata —` ile \
                 bildir (bu projede ajanların çalıştırabileceği bir kurulum komutu yok; \
                 kurulum kullanıcının işi)."
                    .to_string()
            } else {
                format!(
                    "Bağımlılık kurmak için yalnızca şu komutları AYNEN (ek argüman \
                     olmadan) çalıştırabilirsin: {installs}. Yeni bir paket gerekiyorsa \
                     önce ilgili package.json'a ekle, sonra argümansız kurulum komutunu \
                     çalıştır; komuta paket adı yazma (`npm install <paket>` reddedilir). \
                     Başka bir komut gerekiyorsa coordinator'a `hata —` ile bildir."
                )
            };
            format!(
                "{base} Proje klasörü içindeki dosyaları değiştirebilirsin (dışarısı \
                 reddedilir). `Bash` ile yalnızca doğrulama komutlarını çalıştırabilirsin: \
                 {}.{exact} {git} {NO_CD} {install}",
                commands
            )
        }
    }
}

/// Quote `args` for the pane command string (`sidecar::terminal`'s
/// POSIX-style tokenizer): each in double quotes, with `\` and `"`
/// escaped — the tokenizer reads `\\` and `\"` inside double quotes as
/// one literal character, so a Windows path (`\\?\C:\…`, a trailing
/// `\`) arrives intact. Rules contain neither character.
pub(crate) fn join_quoted(args: &[String]) -> String {
    args.iter()
        .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm_term::hierarchy::AGENT_IDS;

    fn none() -> ProjectFacts {
        ProjectFacts::default()
    }

    #[test]
    fn every_agent_has_a_role_and_nobody_gets_an_unrestricted_shell() {
        for &agent in AGENT_IDS {
            let rules = allowed_tools(agent, "swarm-term-01X", &none());
            assert!(
                !rules.iter().any(|r| r == "Bash" || r == "PowerShell"),
                "{agent} has an unrestricted shell rule"
            );
            assert!(!rules.iter().any(|r| r == "Read"), "{agent} has an unscoped Read");
            assert!(!rules.iter().any(|r| r == "Grep" || r == "Glob"), "{agent} has an unscoped search");
            assert!(rules.contains(&format!(
                "Edit(./.bridgespace/swarm-term-01X/outbox/{agent}/**)"
            )));
        }
    }

    #[test]
    fn only_builders_may_write_the_project() {
        for &agent in AGENT_IDS {
            let writes_project = allowed_tools(agent, "s", &none()).contains(&"Edit(./**)".to_string());
            assert_eq!(writes_project, role_of(agent) == Role::Build, "{agent}");
        }
    }

    #[test]
    fn only_builders_may_install_and_only_the_exact_lockfile_install() {
        let facts = ProjectFacts {
            install_commands: vec!["npm ci".into(), "npm --prefix backend ci".into()],
            ..ProjectFacts::default()
        };
        for &agent in AGENT_IDS {
            let rules = allowed_tools(agent, "s", &facts);
            let installs: Vec<&String> = rules.iter().filter(|r| r.contains(" ci")).collect();
            if role_of(agent) == Role::Build {
                // Exact rules: no `:*` / ` *` tail that would admit
                // `npm ci --prefix ../elsewhere`.
                assert_eq!(
                    installs,
                    ["Bash(npm ci)", "PowerShell(npm ci)", "Bash(npm --prefix backend ci)", "PowerShell(npm --prefix backend ci)"],
                    "{agent}"
                );
                assert!(describe_for_persona(agent, &facts).contains("`npm --prefix backend ci`"));
            } else {
                assert!(installs.is_empty(), "{agent}: {installs:?}");
            }
        }
        // The verifiers are told who installs: the builder, or with no
        // lockfile the user.
        assert!(describe_for_persona("integration-tester", &facts).contains("builder `npm ci`"));
        assert!(describe_for_persona("integration-tester", &none()).contains("kullanıcının işi"));
        assert!(describe_for_persona("coordinator", &facts).contains("kurulumu builder'a dispatch et"));
        assert!(!describe_for_persona("coordinator", &none()).contains("kurulumu"));
    }

    #[test]
    fn a_builder_may_run_a_bare_npm_install_but_never_name_a_package() {
        // Exact rules: `npm install left-pad` (a package an injected
        // instruction picked) stays denied; a new dependency goes through
        // package.json, where the reviewers see it.
        let facts =
            ProjectFacts { install_commands: vec!["npm --prefix backend install".into()], ..ProjectFacts::default() };
        for &agent in AGENT_IDS {
            let rules: Vec<String> =
                allowed_tools(agent, "s", &facts).into_iter().filter(|r| r.contains(" install")).collect();
            if role_of(agent) == Role::Build {
                assert_eq!(rules, ["Bash(npm --prefix backend install)", "PowerShell(npm --prefix backend install)"], "{agent}");
                let text = describe_for_persona(agent, &facts);
                assert!(text.contains("package.json") && text.contains("`npm install <paket>`"), "{agent}: {text}");
            } else {
                assert!(rules.is_empty(), "{agent}: {rules:?}");
            }
        }
    }

    #[test]
    fn exact_commands_are_exact_rules_for_the_shell_roles_only() {
        // tsc takes `--outDir` and `--noEmit false`: a rule with room for
        // arguments would let a verifier compile into any folder.
        let tsc = "node backend/node_modules/typescript/bin/tsc -p backend --noEmit";
        let facts = ProjectFacts { exact_commands: vec![tsc.into()], ..ProjectFacts::default() };
        for &agent in AGENT_IDS {
            let rules: Vec<String> =
                allowed_tools(agent, "s", &facts).into_iter().filter(|r| r.contains("typescript/bin/tsc")).collect();
            if role_of(agent) == Role::Coordinate {
                assert!(rules.is_empty(), "{agent}: {rules:?}");
            } else {
                assert_eq!(rules, [format!("Bash({tsc})"), format!("PowerShell({tsc})")], "{agent}");
                let text = describe_for_persona(agent, &facts);
                assert!(text.contains(&format!("`{tsc}`")) && text.contains("AYNEN"), "{agent}: {text}");
            }
            for rule in &rules {
                assert!(!rule.contains('"') && !rule.contains('\\'), "{agent}: {rule}");
            }
        }
    }

    #[test]
    fn coordinators_have_no_shell_verifiers_do() {
        let has_bash = |a: &str| allowed_tools(a, "s", &none()).iter().any(|r| r.starts_with("Bash("));
        for a in ["orchestrator", "coordinator", "scout", "planner"] {
            assert!(!has_bash(a), "{a}");
        }
        for a in ["backend-reviewer", "frontend-reviewer", "integration-tester", "backend-builder"] {
            assert!(has_bash(a), "{a}");
        }
        assert!(allowed_tools("integration-tester", "s", &none()).contains(&"Bash(cargo test:*)".to_string()));
    }

    #[test]
    fn args_use_dont_ask_and_quote_cleanly() {
        let args =
            claude_permission_args(SwarmTermPermissionMode::Restricted, "scout", "swarm-term-01X", &none());
        assert_eq!(&args[..3], &["--permission-mode", "dontAsk", "--allowedTools"]);
        let joined = join_quoted(&args);
        assert!(joined.contains("\"Read(./**)\""));
        assert!(!args.iter().any(|a| a.contains('"') || a.contains('\\')));
    }

    #[test]
    fn quoting_keeps_windows_paths_and_quotes_intact() {
        let args = vec![
            r"C:\Users\x\AppData\Roaming\app\sessions\s\scout.persona.md".to_string(),
            r"\\?\C:\dir\".to_string(),
            r#"say "hi""#.to_string(),
        ];
        assert_eq!(
            join_quoted(&args),
            r#""C:\\Users\\x\\AppData\\Roaming\\app\\sessions\\s\\scout.persona.md" "\\\\?\\C:\\dir\\" "say \"hi\"""#
        );
    }

    #[test]
    fn the_deny_list_follows_the_allow_list() {
        for &agent in AGENT_IDS {
            let args = claude_permission_args(SwarmTermPermissionMode::Restricted, agent, "s", &none());
            let deny_at = args.iter().position(|a| a == "--disallowedTools").expect(agent);
            assert_eq!(args.iter().filter(|a| *a == "--disallowedTools").count(), 1, "{agent}");
            assert_eq!(&args[3..deny_at], allowed_tools(agent, "s", &none()).as_slice(), "{agent}");
            assert_eq!(&args[deny_at + 1..], denied_tools(agent, "s").as_slice(), "{agent}");
        }
    }

    #[test]
    fn nobody_writes_vcs_or_tool_config_or_the_bridge_bookkeeping() {
        for &agent in AGENT_IDS {
            let deny = denied_tools(agent, "swarm-term-01X");
            for rule in [
                "Edit(./.git/**)",
                "Edit(./.claude/**)",
                "Edit(./.vscode/**)",
                "Edit(./.idea/**)",
                "Edit(./.bridgespace/.gitignore)",
                "Edit(./.bridgespace/_archive/**)",
                "Edit(./.bridgespace/swarm-term-01X/processed/**)",
                "Edit(./.bridgespace/swarm-term-01X/rejected/**)",
                "Edit(./.bridgespace/swarm-term-01X/inbox/**)",
                "WebFetch",
                "WebSearch",
            ] {
                assert!(deny.iter().any(|r| r == rule), "{agent} lacks {rule}");
            }
        }
    }

    #[test]
    fn every_role_is_denied_the_common_secret_files() {
        // Even the read-only coordinate roles (whose only write is their
        // own outbox) must not read the user's secrets through Read(./**).
        for &agent in AGENT_IDS {
            let deny = denied_tools(agent, "s");
            for rule in ["Read(./.env)", "Read(./**/.env.local)", "Read(./**/id_rsa)", "Read(./secrets/**)"] {
                assert!(deny.iter().any(|r| r == rule), "{agent} lacks {rule}");
            }
        }
    }

    #[test]
    fn every_other_outbox_is_denied_and_the_own_one_never() {
        use std::collections::BTreeSet;
        for &agent in AGENT_IDS {
            // The exact set of bridge rules: nothing broader may swallow
            // the agent's own outbox (deny beats allow, so it would
            // silence the agent), and every other outbox is covered.
            let bridge: BTreeSet<String> = denied_tools(agent, "s")
                .into_iter()
                .filter(|r| r.contains(".bridgespace"))
                .collect();
            let mut expected: BTreeSet<String> =
                [".gitignore", "_archive/**", "experience.md", "s/processed/**", "s/rejected/**", "s/inbox/**"]
                    .iter()
                    .map(|p| format!("Edit(./.bridgespace/{p})"))
                    .collect();
            for other in AGENT_IDS.iter().filter(|other| **other != agent) {
                expected.insert(format!("Edit(./.bridgespace/s/outbox/{other}/**)"));
            }
            assert_eq!(bridge, expected, "{agent}");
        }
    }

    #[test]
    fn shell_roles_lose_the_escaping_git_options_coordinators_lose_the_shell() {
        for &agent in AGENT_IDS {
            let deny = denied_tools(agent, "s");
            let has = |r: &str| deny.iter().any(|d| d == r);
            if role_of(agent) == Role::Coordinate {
                assert!(has("Bash") && has("PowerShell"), "{agent}");
            } else {
                assert!(!has("Bash") && !has("PowerShell"), "{agent}");
                for rule in [
                    "Bash(git diff *--output*)",
                    "Bash(git log *--output*)",
                    "Bash(git show *--output*)",
                    "Bash(git diff *--no-index*)",
                    "Bash(git diff *--ext-diff*)",
                    "PowerShell(git diff *--output*)",
                    "PowerShell(git diff *--no-index*)",
                    // The out-of-project two-path read forms.
                    "Bash(git diff *:/*)",
                    "Bash(git diff *~/*)",
                    "Bash(git diff *../*)",
                    "PowerShell(git diff *../*)",
                ] {
                    assert!(has(rule), "{agent} lacks {rule}");
                }
                // No deny rule carries a backslash (claude treats it as a
                // glob escape; the pane tokenizer would also mangle it).
                for rule in &deny {
                    assert!(!rule.contains('\\'), "{agent}: backslash in {rule}");
                }
            }
        }
    }

    #[test]
    fn shell_roles_lose_the_output_path_options_but_keep_their_commands() {
        let facts = ProjectFacts {
            commands: vec![
                "pnpm -C app test".into(),
                "pnpm -C app exec vitest run".into(),
                "npm --prefix backend run test:unit".into(),
            ],
            install_commands: vec![
                "npm ci".into(),
                "npm --prefix backend install".into(),
                "pnpm install --frozen-lockfile".into(),
            ],
            exact_commands: vec!["node app/node_modules/typescript/bin/tsc -p app --noEmit".into()],
            ..ProjectFacts::default()
        };
        for &agent in AGENT_IDS {
            let deny = denied_tools(agent, "s");
            let has = |r: &str| deny.iter().any(|d| d == r);
            if role_of(agent) == Role::Coordinate {
                assert!(!has("Bash(*--target-dir*)"), "{agent}");
                continue;
            }
            for rule in [
                "Bash(*--target-dir*)",
                "Bash(*--outDir*)",
                "Bash(*--outputFile*)",
                "Bash(*--junitxml*)",
                "Bash(* -o *)",
                "Bash(*-exec *)",
                "Bash(*--manifest-path*)",
                "PowerShell(*--target-dir*)",
                "PowerShell(*-coverprofile*)",
            ] {
                assert!(has(rule), "{agent} lacks {rule}");
            }
            // No option in the list hits a command the role may run: a deny
            // beats an allow, so one collision would silently kill a
            // project's own test or install command.
            for allowed in allowed_tools(agent, "s", &facts) {
                for opt in OUTPUT_ESCAPES {
                    assert!(!allowed.contains(opt), "{agent}: `{opt}` would deny {allowed}");
                }
            }
            let text = describe_for_persona(agent, &facts);
            assert!(text.contains("--target-dir") && text.contains("`-o`"), "{agent}: {text}");
        }
    }

    #[test]
    fn nobody_writes_the_experience_file() {
        // Neuron's memory of the project feeds the next session's
        // personas: an agent that could edit it would write tomorrow's
        // instructions.
        for &agent in AGENT_IDS {
            let deny = denied_tools(agent, "s");
            assert!(deny.contains(&"Edit(./.bridgespace/experience.md)".to_string()), "{agent}: {deny:?}");
        }
    }

    #[test]
    fn no_rule_needs_quoting_the_pane_tokenizer_cannot_do() {
        for &agent in AGENT_IDS {
            for rule in allowed_tools(agent, "s", &none()).iter().chain(denied_tools(agent, "s").iter()) {
                assert!(!rule.contains('"') && !rule.contains('\\'), "{agent}: {rule}");
            }
        }
    }

    #[test]
    fn bypass_mode_drops_the_allowlist() {
        assert_eq!(
            claude_permission_args(SwarmTermPermissionMode::Bypass, "backend-builder", "s", &none()),
            ["--permission-mode", "bypassPermissions"]
        );
    }

    #[test]
    fn only_the_bypass_value_opts_out() {
        use SwarmTermPermissionMode::{Bypass, Restricted};
        assert_eq!(mode_from(None), Restricted);
        assert_eq!(mode_from(Some("")), Restricted);
        assert_eq!(mode_from(Some("off")), Restricted);
        assert_eq!(mode_from(Some("bypassPermissions")), Restricted);
        assert_eq!(mode_from(Some("bypass")), Bypass);
        assert_eq!(mode_from(Some("  BYPASS \n")), Bypass);
    }

    #[test]
    fn mode_serializes_for_the_ui() {
        assert_eq!(
            serde_json::to_string(&SwarmTermPermissionMode::Restricted).unwrap(),
            "\"restricted\""
        );
        assert_eq!(
            serde_json::to_string(&SwarmTermPermissionMode::Bypass).unwrap(),
            "\"bypass\""
        );
    }

    #[test]
    fn persona_summary_matches_the_role() {
        assert!(describe_for_persona("scout", &none()).contains("shell (`Bash`) komutu"));
        assert!(describe_for_persona("backend-reviewer", &none()).contains("cargo test"));
        assert!(describe_for_persona("backend-builder", &none()).contains("değiştirebilirsin"));
        let tester = describe_for_persona("integration-tester", &none());
        assert!(tester.contains("DIŞINA `cd` YAPMA"));
        // In-project subdirectories stay usable (review finding).
        assert!(tester.contains("`cd <alt-klasör> && <komut>`"));
        assert!(!describe_for_persona("scout", &none()).contains("DIŞINA `cd`"));
        // The deny list is announced, so agents don't learn it by denial.
        for &agent in AGENT_IDS {
            let text = describe_for_persona(agent, &none());
            assert!(text.contains("başka bir ajanın outbox'u"), "{agent}");
            assert!(text.contains("`.git/`"), "{agent}");
            assert_eq!(text.contains("`--output`"), role_of(agent) != Role::Coordinate, "{agent}");
        }
    }
}

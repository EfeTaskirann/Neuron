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
//!     own read-only commands such as `echo`, scoped to the project.)
//!
//! Residual risk, by design: running a project's tests / build executes
//! that project's code, and an agent can still write into another
//! agent's outbox inside `.bridgespace` (builders) — see the bridge's
//! trust model. Setting [`PERMISSIONS_ENV`]`=bypass` restores the old
//! unrestricted mode for every pane (logged as a warning).

/// Opt-out: `NEURON_TERM_PERMISSIONS=bypass` → every pane runs with
/// `bypassPermissions` again.
pub(crate) const PERMISSIONS_ENV: &str = "NEURON_TERM_PERMISSIONS";

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
/// (prefix match). Covers the common toolchains a swarm may meet.
const VERIFY_COMMANDS: &[&str] = &[
    "cargo check",
    "cargo test",
    "cargo clippy",
    "cargo build",
    "cargo fmt --check",
    "pnpm test",
    "pnpm typecheck",
    "pnpm lint",
    "pnpm -C app test",
    "pnpm -C app typecheck",
    "pnpm -C app lint",
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

/// The `claude` CLI arguments that set `agent_id`'s permissions for a
/// session whose bridge root is `.bridgespace/<session_id>/` under the
/// pane's working directory (the project).
pub(crate) fn claude_permission_args(agent_id: &str, session_id: &str) -> Vec<String> {
    if bypass_requested() {
        return vec!["--permission-mode".into(), "bypassPermissions".into()];
    }
    let mut args = vec!["--permission-mode".into(), "dontAsk".into(), "--allowedTools".into()];
    args.extend(allowed_tools(agent_id, session_id));
    args
}

fn bypass_requested() -> bool {
    std::env::var(PERMISSIONS_ENV)
        .is_ok_and(|v| v.trim().eq_ignore_ascii_case("bypass"))
}

/// True when the opt-out is active (for a one-time startup warning).
pub(crate) fn bypass_active() -> bool {
    bypass_requested()
}

/// Pure: the allowlist for one agent.
pub(crate) fn allowed_tools(agent_id: &str, session_id: &str) -> Vec<String> {
    let mut rules = vec![
        "Read(./**)".to_string(),
        "TodoWrite".to_string(),
        format!("Edit(./.bridgespace/{session_id}/outbox/{agent_id}/**)"),
    ];
    let role = role_of(agent_id);
    if role == Role::Build {
        rules.push("Edit(./**)".to_string());
    }
    if matches!(role, Role::Verify | Role::Build) {
        // claude has a separate PowerShell tool on Windows; the same
        // commands must be reachable through it, with the same limits.
        for c in VERIFY_COMMANDS {
            rules.push(format!("Bash({c}:*)"));
            rules.push(format!("PowerShell({c})"));
            rules.push(format!("PowerShell({c} *)"));
        }
    }
    rules
}

/// Shell commands start in the project root already; a `cd` OUT of the
/// project (live run: the tester `cd`-ed to the parent repository) gets
/// the whole command denied. A `cd` into a project subdirectory is
/// allowed and sometimes needed (a package without a `-C` variant).
const NO_CD: &str = "Shell zaten proje kökünde açılır: komutu doğrudan çalıştır (ör. \
                     `cargo test`). Proje kökünün DIŞINA `cd` YAPMA — o komut reddedilir. \
                     Proje içindeki bir alt klasör gerekiyorsa `-C <klasör>` biçimini ya da \
                     `cd <alt-klasör> && <komut>` kullan.";

/// Human-readable summary for the persona footer, so an agent knows its
/// limits up front instead of discovering them through denied calls.
pub(crate) fn describe_for_persona(agent_id: &str) -> String {
    let base = "Proje dosyalarını OKUYABİLİRSİN (yalnızca proje klasörü içinde; \
                dışarısı reddedilir). Mesajlarını yalnızca KENDİ outbox'una yazabilirsin.";
    match role_of(agent_id) {
        Role::Coordinate => format!(
            "{base} Proje dosyalarını değiştiremez, shell (`Bash`) komutu ve web \
             aracı kullanamazsın. Değişiklik gerekiyorsa bir builder'a dispatch et."
        ),
        Role::Verify => format!(
            "{base} Proje dosyalarını değiştiremezsin. `Bash` ile yalnızca doğrulama \
             komutlarını çalıştırabilirsin: {}. {NO_CD}",
            VERIFY_COMMANDS.join(", ")
        ),
        Role::Build => format!(
            "{base} Proje klasörü içindeki dosyaları değiştirebilirsin (dışarısı \
             reddedilir). `Bash` ile yalnızca doğrulama komutlarını çalıştırabilirsin: \
             {}. {NO_CD} Paket kurmak / başka bir komut gerekiyorsa coordinator'a \
             `hata —` ile bildir.",
            VERIFY_COMMANDS.join(", ")
        ),
    }
}

/// Quote `args` for the pane command string (`sidecar::terminal`'s
/// POSIX-style tokenizer). Rules contain spaces / parentheses but never
/// `"` or `\`.
pub(crate) fn join_quoted(args: &[String]) -> String {
    args.iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm_term::hierarchy::AGENT_IDS;

    #[test]
    fn every_agent_has_a_role_and_nobody_gets_an_unrestricted_shell() {
        for &agent in AGENT_IDS {
            let rules = allowed_tools(agent, "swarm-term-01X");
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
            let writes_project = allowed_tools(agent, "s").contains(&"Edit(./**)".to_string());
            assert_eq!(writes_project, role_of(agent) == Role::Build, "{agent}");
        }
    }

    #[test]
    fn coordinators_have_no_shell_verifiers_do() {
        let has_bash = |a: &str| allowed_tools(a, "s").iter().any(|r| r.starts_with("Bash("));
        for a in ["orchestrator", "coordinator", "scout", "planner"] {
            assert!(!has_bash(a), "{a}");
        }
        for a in ["backend-reviewer", "frontend-reviewer", "integration-tester", "backend-builder"] {
            assert!(has_bash(a), "{a}");
        }
        assert!(allowed_tools("integration-tester", "s").contains(&"Bash(cargo test:*)".to_string()));
    }

    #[test]
    fn args_use_dont_ask_and_quote_cleanly() {
        let args = claude_permission_args("scout", "swarm-term-01X");
        if !bypass_active() {
            assert_eq!(&args[..3], &["--permission-mode", "dontAsk", "--allowedTools"]);
        }
        let joined = join_quoted(&args);
        assert!(joined.contains("\"Read(./**)\""));
        assert!(!args.iter().any(|a| a.contains('"') || a.contains('\\')));
    }

    #[test]
    fn persona_summary_matches_the_role() {
        assert!(describe_for_persona("scout").contains("shell (`Bash`) komutu"));
        assert!(describe_for_persona("backend-reviewer").contains("cargo test"));
        assert!(describe_for_persona("backend-builder").contains("değiştirebilirsin"));
        let tester = describe_for_persona("integration-tester");
        assert!(tester.contains("DIŞINA `cd` YAPMA"));
        // In-project subdirectories stay usable (review finding).
        assert!(tester.contains("`cd <alt-klasör> && <komut>`"));
        assert!(!describe_for_persona("scout").contains("DIŞINA `cd`"));
    }
}

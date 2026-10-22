//! What the swarm knows about a project before an agent reads a file:
//! its toolchains and the commands that test / typecheck / lint it, read
//! from the manifests in the project root and one folder down (a
//! monorepo's packages). The facts go into every persona footer
//! ("Proje bilgisi") and the commands into the shell roles' allowlist —
//! in place of one project's layout written into the personas.

use std::path::Path;

/// Node scripts worth a verifier's time (run, not built).
const NODE_SCRIPTS: &[&str] = &["test", "typecheck", "type-check", "lint", "check"];

/// Folders one level down that are never a package of the project.
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", "vendor", "coverage"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectFacts {
    /// Toolchains, readable: `Rust (Cargo)`, `Node (pnpm)`, …
    pub stacks: Vec<String>,
    /// Commands that verify the project, as the agents should type them.
    pub commands: Vec<String>,
    /// Commands that install a package's dependencies, which the builders
    /// may run exactly as written: `npm ci` (what the package's own
    /// lockfile pins) and a bare `npm install` (what its package.json
    /// lists). Never with a package name: a new dependency goes through
    /// package.json, where the reviewers see it.
    pub install_commands: Vec<String>,
    /// Verification commands that run only exactly as written: a
    /// TypeScript package's own compiler, for a package with no
    /// typecheck script. `tsc` takes `--outDir` and `--noEmit false`, so
    /// a rule with room for arguments would let it write anywhere.
    pub exact_commands: Vec<String>,
    /// Verification commands the start-up probe (`preflight`) found
    /// failing before any agent touched the project, as `<command>
    /// (exit <code>)`. Told to the agents so they don't take a
    /// pre-existing failure for their own.
    pub broken: Vec<String>,
    /// What earlier sessions in this project learned, one line per card
    /// (see `experience::lines_for_persona`); information, never an
    /// instruction.
    pub experience: Vec<String>,
}

impl ProjectFacts {
    fn add_stack(&mut self, stack: &str) {
        if !self.stacks.iter().any(|s| s == stack) {
            self.stacks.push(stack.to_string());
        }
    }

    fn add_command(&mut self, command: String) {
        if !self.commands.contains(&command) {
            self.commands.push(command);
        }
    }

    fn add_install_command(&mut self, command: String) {
        if !self.install_commands.contains(&command) {
            self.install_commands.push(command);
        }
    }

    fn add_exact_command(&mut self, command: String) {
        if !self.exact_commands.contains(&command) {
            self.exact_commands.push(command);
        }
    }
}

/// Read the facts of the project at `project` (best-effort: anything
/// unreadable is simply not a fact).
pub(crate) fn detect(project: &Path) -> ProjectFacts {
    let mut facts = ProjectFacts::default();
    let root_pm = node_package_manager(project);
    if project.join("Cargo.toml").is_file() {
        facts.add_stack("Rust (Cargo)");
        for c in ["cargo check", "cargo test", "cargo clippy"] {
            facts.add_command(c.to_string());
        }
    }
    node_package(project, None, root_pm, false, &mut facts);
    let npm_workspace = read_manifest(project).is_some_and(|m| m.get("workspaces").is_some());
    if ["pyproject.toml", "pytest.ini", "setup.cfg", "setup.py", "tox.ini"]
        .iter()
        .any(|f| project.join(f).is_file())
    {
        facts.add_stack("Python");
        facts.add_command("pytest".into());
    }
    if project.join("go.mod").is_file() {
        facts.add_stack("Go");
        facts.add_command("go test ./...".into());
        facts.add_command("go vet ./...".into());
    }
    // One level down: the packages of a monorepo.
    let mut subdirs: Vec<String> = std::fs::read_dir(project)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| safe_dir_name(name) && !SKIP_DIRS.contains(&name.as_str()))
        .collect();
    subdirs.sort();
    for dir in subdirs {
        let path = project.join(&dir);
        if path.join("Cargo.toml").is_file() && !project.join("Cargo.toml").is_file() {
            facts.add_stack("Rust (Cargo)");
            for c in ["check", "test", "clippy"] {
                facts.add_command(format!("cargo {c} --manifest-path {dir}/Cargo.toml"));
            }
        }
        let pm = node_package_manager(&path).or(root_pm);
        node_package(&path, Some(&dir), pm, npm_workspace, &mut facts);
    }
    facts
}

/// A folder name that can sit inside a permission rule as-is: no
/// spaces, quotes, parentheses or path separators, not hidden.
fn safe_dir_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The package manager a folder's lockfile names.
fn node_package_manager(dir: &Path) -> Option<&'static str> {
    [
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
        ("package-lock.json", "npm"),
    ]
    .into_iter()
    .find(|(lock, _)| dir.join(lock).is_file())
    .map(|(_, pm)| pm)
}

/// The `package.json` in `dir`, parsed.
fn read_manifest(dir: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// The verification and install commands of the `package.json` in `path`
/// (`subdir` = its folder below the project root, `None` for the root;
/// `workspace_member` = the root is an npm workspace).
fn node_package(
    path: &Path,
    subdir: Option<&str>,
    pm: Option<&'static str>,
    workspace_member: bool,
    facts: &mut ProjectFacts,
) {
    let Some(manifest) = read_manifest(path) else {
        return;
    };
    let pm = pm.unwrap_or("npm");
    facts.add_stack(&format!("Node ({pm})"));
    // `npm ci` needs the lockfile in the package's own folder (a
    // workspace root's lockfile does not let `npm --prefix <pkg> ci` run).
    if pm == "npm" && ["package-lock.json", "npm-shrinkwrap.json"].iter().any(|f| path.join(f).is_file()) {
        facts.add_install_command(match subdir {
            None => "npm ci".to_string(),
            Some(dir) => format!("npm --prefix {dir} ci"),
        });
    }
    // A bare `npm install` (owner decision, 2026-10-20): with no package
    // name it installs what package.json lists and syncs the lockfile —
    // how a builder sets up a package with no lockfile (the live Medusa
    // backend) or installs a dependency it added; `npm ci` refuses both.
    // Not for a workspace member: `npm --prefix <member> install` would
    // set it up as a project of its own.
    if pm == "npm" && !workspace_member {
        facts.add_install_command(match subdir {
            None => "npm install".to_string(),
            Some(dir) => format!("npm --prefix {dir} install"),
        });
    }
    let scripts = manifest.get("scripts").and_then(|s| s.as_object());
    if let Some(scripts) = scripts {
        for base in NODE_SCRIPTS {
            if scripts.contains_key(*base) {
                facts.add_command(node_command(pm, subdir, base));
            }
            // Variants of a base script (`test:unit`, `lint:css`): a package
            // may have only those (live: a Medusa backend with `test:unit`
            // and no `test`, so its tests could not be run at all).
            let mut variants: Vec<&str> = scripts
                .keys()
                .map(String::as_str)
                .filter(|name| is_script_variant(base, name))
                .collect();
            variants.sort_unstable();
            for name in variants {
                facts.add_command(node_command(pm, subdir, name));
            }
        }
    }
    // A TypeScript package no script typechecks (live: the same Medusa
    // backend): its own compiler, run exactly — never through `npx`,
    // which fetches a package named `tsc` from the registry when the
    // local one is missing.
    let typechecked = scripts.is_some_and(|s| s.keys().any(|name| is_typecheck_script(name)));
    if !typechecked && path.join("tsconfig.json").is_file() && declares(&manifest, "typescript") {
        facts.add_exact_command(match subdir {
            None => "node node_modules/typescript/bin/tsc --noEmit".to_string(),
            Some(dir) => format!("node {dir}/node_modules/typescript/bin/tsc -p {dir} --noEmit"),
        });
    }
}

/// A script that already typechecks its package.
fn is_typecheck_script(name: &str) -> bool {
    ["typecheck", "type-check"].iter().any(|base| name == *base || is_script_variant(base, name))
}

/// `manifest` lists `dependency` in its dependencies or devDependencies.
fn declares(manifest: &serde_json::Value, dependency: &str) -> bool {
    ["dependencies", "devDependencies"].iter().any(|field| {
        manifest.get(field).and_then(|d| d.as_object()).is_some_and(|d| d.contains_key(dependency))
    })
}

/// `name` is `<base>:<variant>` with a variant that can sit inside a
/// permission rule as-is, and is not a watch mode (it never exits, so a
/// verifier running it would hang).
fn is_script_variant(base: &str, name: &str) -> bool {
    name.strip_prefix(base)
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|variant| {
            !variant.is_empty()
                && !variant.contains("watch")
                && variant.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        })
}

/// How `pm` runs `script` in `subdir` (`None` = the project root).
fn node_command(pm: &str, subdir: Option<&str>, script: &str) -> String {
    let run = |direct: bool| if direct { script.to_string() } else { format!("run {script}") };
    match (pm, subdir) {
        ("pnpm", None) => format!("pnpm {script}"),
        ("pnpm", Some(dir)) => format!("pnpm -C {dir} {script}"),
        ("yarn", None) => format!("yarn {script}"),
        ("yarn", Some(dir)) => format!("yarn --cwd {dir} {script}"),
        ("bun", None) => format!("bun run {script}"),
        ("bun", Some(dir)) => format!("bun run --cwd {dir} {script}"),
        (_, None) => format!("npm {}", run(script == "test")),
        (_, Some(dir)) => format!("npm --prefix {dir} {}", run(script == "test")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_rust_and_pnpm_monorepo_like_neuron() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "Cargo.toml", "[workspace]");
        write(d.path(), "package.json", r#"{"scripts": {"dev": "x", "test": "x", "lint": "x"}}"#);
        write(d.path(), "pnpm-lock.yaml", "");
        write(d.path(), "app/package.json", r#"{"scripts": {"test": "vitest", "typecheck": "tsc", "lint": "eslint", "build": "vite"}}"#);
        write(d.path(), "src-tauri/Cargo.toml", "[package]");
        write(d.path(), "node_modules/x/package.json", r#"{"scripts": {"test": "x"}}"#);
        let facts = detect(d.path());
        assert_eq!(facts.stacks, ["Rust (Cargo)", "Node (pnpm)"]);
        assert_eq!(
            facts.commands,
            [
                "cargo check",
                "cargo test",
                "cargo clippy",
                "pnpm test",
                "pnpm lint",
                "pnpm -C app test",
                "pnpm -C app typecheck",
                "pnpm -C app lint",
            ]
        );
    }

    #[test]
    fn npm_python_go_and_a_nested_crate() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts": {"test": "jest", "typecheck": "tsc"}}"#);
        write(d.path(), "package-lock.json", "{}");
        write(d.path(), "pyproject.toml", "");
        write(d.path(), "go.mod", "module x");
        write(d.path(), "core/Cargo.toml", "[package]");
        write(d.path(), "web/package.json", r#"{"scripts": {"lint": "eslint"}}"#);
        let facts = detect(d.path());
        assert_eq!(facts.stacks, ["Node (npm)", "Python", "Go", "Rust (Cargo)"]);
        assert_eq!(
            facts.commands,
            [
                "npm test",
                "npm run typecheck",
                "pytest",
                "go test ./...",
                "go vet ./...",
                "cargo check --manifest-path core/Cargo.toml",
                "cargo test --manifest-path core/Cargo.toml",
                "cargo clippy --manifest-path core/Cargo.toml",
                "npm --prefix web run lint",
            ]
        );
    }

    #[test]
    fn an_unknown_project_has_no_facts_and_odd_folders_are_never_rules() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "notes.txt", "");
        assert_eq!(detect(d.path()), ProjectFacts::default());
        // A folder name that would break out of a permission rule is skipped.
        write(d.path(), "x) Bash(y/package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), ".hidden/package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), "ok-pkg/package.json", r#"{"scripts": {"test": "x"}}"#);
        assert_eq!(detect(d.path()).commands, ["npm --prefix ok-pkg test"]);
        assert!(!safe_dir_name("a b") && !safe_dir_name("a\"b") && safe_dir_name("my_pkg.v2"));
    }

    #[test]
    fn script_variants_like_a_medusa_backend() {
        let d = tempfile::tempdir().unwrap();
        write(
            d.path(),
            "backend/package.json",
            r#"{"scripts": {"build": "x", "test:unit": "jest", "test:integration:http": "jest",
                "test:watch": "jest --watch", "lint:css": "x", "test:": "x", "test:a b": "x", "testx": "x"}}"#,
        );
        write(d.path(), "storefront/package.json", r#"{"scripts": {"test": "vitest run", "lint": "next lint"}}"#);
        assert_eq!(
            detect(d.path()).commands,
            [
                "npm --prefix backend run test:integration:http",
                "npm --prefix backend run test:unit",
                "npm --prefix backend run lint:css",
                "npm --prefix storefront test",
                "npm --prefix storefront run lint",
            ]
        );
        assert_eq!(node_command("pnpm", None, "test:unit"), "pnpm test:unit");
    }

    #[test]
    fn only_a_package_with_its_own_npm_lockfile_gets_npm_ci() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), "package-lock.json", "{}");
        write(d.path(), "api/package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), "api/npm-shrinkwrap.json", "{}");
        // Inherits npm from the root lockfile, but has none of its own.
        write(d.path(), "web/package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), "pn/package.json", r#"{"scripts": {"test": "x"}}"#);
        write(d.path(), "pn/pnpm-lock.yaml", "");
        let ci: Vec<String> = detect(d.path()).install_commands.into_iter().filter(|c| c.ends_with(" ci")).collect();
        assert_eq!(ci, ["npm ci", "npm --prefix api ci"]);
    }

    #[test]
    fn every_npm_package_gets_a_bare_install_but_not_inside_an_npm_workspace() {
        // The live Medusa backend had no lockfile, so `npm ci` could not
        // set it up — and a builder who adds a dependency cannot run it
        // either (package.json and the lockfile disagree). A bare install
        // covers both; a pnpm package gets no npm command.
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "backend/package.json", r#"{"scripts": {"test:unit": "jest"}}"#);
        write(d.path(), "storefront/package.json", r#"{"scripts": {"test": "vitest"}}"#);
        write(d.path(), "storefront/package-lock.json", "{}");
        write(d.path(), "web/package.json", "{}");
        write(d.path(), "web/pnpm-lock.yaml", "");
        assert_eq!(
            detect(d.path()).install_commands,
            ["npm --prefix backend install", "npm --prefix storefront ci", "npm --prefix storefront install"]
        );
        // An npm workspace installs at its root only: `npm --prefix app
        // install` would set the member up as a project of its own.
        let ws = tempfile::tempdir().unwrap();
        write(ws.path(), "package.json", r#"{"workspaces": ["app"]}"#);
        write(ws.path(), "package-lock.json", "{}");
        write(ws.path(), "app/package.json", r#"{"scripts": {"test": "x"}}"#);
        assert_eq!(detect(ws.path()).install_commands, ["npm ci", "npm install"]);
    }

    #[test]
    fn a_typescript_package_without_a_typecheck_script_gets_its_own_tsc() {
        // The live Medusa backend: TypeScript and a tsconfig, but only
        // `test:unit` — so nothing in the swarm could typecheck it.
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "backend/package.json", r#"{"scripts": {"test:unit": "jest"}, "devDependencies": {"typescript": "^5"}}"#);
        write(d.path(), "backend/tsconfig.json", "{}");
        // Its own typecheck script already covers it: no second, raw tsc.
        write(d.path(), "storefront/package.json", r#"{"scripts": {"typecheck": "tsc --noEmit"}, "devDependencies": {"typescript": "^5"}}"#);
        write(d.path(), "storefront/tsconfig.json", "{}");
        // Without a tsconfig, or without TypeScript, tsc has nothing to run.
        write(d.path(), "tools/package.json", r#"{"devDependencies": {"typescript": "^5"}}"#);
        write(d.path(), "plain/package.json", r#"{"dependencies": {"left-pad": "1"}}"#);
        write(d.path(), "plain/tsconfig.json", "{}");
        assert_eq!(
            detect(d.path()).exact_commands,
            ["node backend/node_modules/typescript/bin/tsc -p backend --noEmit"]
        );
    }

    #[test]
    fn a_root_typescript_package_with_no_scripts_runs_tsc_from_the_root() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "package.json", r#"{"dependencies": {"typescript": "5"}}"#);
        write(d.path(), "tsconfig.json", "{}");
        assert_eq!(detect(d.path()).exact_commands, ["node node_modules/typescript/bin/tsc --noEmit"]);
    }

    #[test]
    fn the_hyphenated_spelling_and_variants_are_typecheck_scripts_too() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "web/package.json", r#"{"scripts": {"type-check": "tsc"}, "devDependencies": {"typescript": "5"}}"#);
        write(d.path(), "web/tsconfig.json", "{}");
        write(d.path(), "api/package.json", r#"{"scripts": {"typecheck:app": "tsc -p app"}, "devDependencies": {"typescript": "5"}}"#);
        write(d.path(), "api/tsconfig.json", "{}");
        let facts = detect(d.path());
        assert_eq!(facts.commands, ["npm --prefix api run typecheck:app", "npm --prefix web run type-check"]);
        // Each is typechecked by its own script: no raw tsc on top.
        assert!(facts.exact_commands.is_empty(), "{:?}", facts.exact_commands);
    }

    #[test]
    fn yarn_and_bun_spellings() {
        assert_eq!(node_command("yarn", Some("app"), "test"), "yarn --cwd app test");
        assert_eq!(node_command("bun", None, "lint"), "bun run lint");
        assert_eq!(node_command("npm", None, "lint"), "npm run lint");
    }
}

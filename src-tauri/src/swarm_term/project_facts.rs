//! What the swarm knows about a project before an agent reads a file:
//! its toolchains and the commands that test / typecheck / lint it, read
//! from the manifests in the project root and one folder down (a
//! monorepo's packages). The facts go into every persona footer
//! ("Proje bilgisi") and the commands into the shell roles' allowlist —
//! in place of one project's layout written into the personas.

use std::path::Path;

/// Node scripts worth a verifier's time (run, not built).
const NODE_SCRIPTS: &[&str] = &["test", "typecheck", "lint", "check"];

/// Folders one level down that are never a package of the project.
const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", "vendor", "coverage"];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectFacts {
    /// Toolchains, readable: `Rust (Cargo)`, `Node (pnpm)`, …
    pub stacks: Vec<String>,
    /// Commands that verify the project, as the agents should type them.
    pub commands: Vec<String>,
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
    node_package(project, None, root_pm, &mut facts);
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
        node_package(&path, Some(&dir), pm, &mut facts);
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

/// The verification scripts of the `package.json` in `path`
/// (`subdir` = its folder below the project root, `None` for the root).
fn node_package(path: &Path, subdir: Option<&str>, pm: Option<&'static str>, facts: &mut ProjectFacts) {
    let Some(manifest) = std::fs::read_to_string(path.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return;
    };
    let pm = pm.unwrap_or("npm");
    facts.add_stack(&format!("Node ({pm})"));
    let Some(scripts) = manifest.get("scripts").and_then(|s| s.as_object()) else {
        return;
    };
    for script in NODE_SCRIPTS.iter().filter(|s| scripts.contains_key(**s)) {
        facts.add_command(node_command(pm, subdir, script));
    }
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
    fn yarn_and_bun_spellings() {
        assert_eq!(node_command("yarn", Some("app"), "test"), "yarn --cwd app test");
        assert_eq!(node_command("bun", None, "lint"), "bun run lint");
        assert_eq!(node_command("npm", None, "lint"), "npm run lint");
    }
}

//! Bundled MCP server manifests.
//!
//! Each manifest describes one seeded server: the catalog metadata
//! (id, name, author, description) and the spawn recipe used by
//! [`crate::mcp::registry`] when the user installs it. Only servers
//! Neuron can actually launch are bundled: `spawn` is required.
//!
//! Manifests are baked into the binary at compile time via
//! `include_str!` so the installer is self-contained — no JSON files
//! ship on disk, and the seed function in [`crate::db`] reads them
//! directly from the embedded constants.
//!
//! ## Adding a new seeded server
//!
//! 1. Drop a JSON file under `src-tauri/src/mcp/manifests/`.
//! 2. Add an `include_str!` line to [`ALL_MANIFESTS_JSON`].
//! 3. Re-run `cargo run --bin export-bindings` (no shape change, but
//!    confirms the binary still builds with the new manifest).
//!
//! Adding a row at runtime is out of scope for Week 2 (Charter §"Out
//! of scope" — no third-party marketplace).

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Spawn recipe for one MCP server. The Week-2 client only ever
/// shells out to `npx`; future kinds (cargo-installed, system PATH,
/// local script) extend this enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ManifestSpawn {
    /// `npx -y <package> <...args>` with `__ROOT__` placeholders
    /// substituted at install time. Charter §"MCP integration" forbids
    /// shipping pre-built binaries inside the Tauri bundle, so this is
    /// the only spawn kind for now.
    Npx {
        package: String,
        #[serde(default)]
        args_template: Vec<String>,
    },
}

/// Where to source the default root path for a server that takes one
/// (currently only Filesystem). The string lives in the manifest so a
/// future `Tempdir` variant for tests does not require a code change.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultRootKind {
    /// A folder of the server's own, `<app data>/mcp/<server id>`,
    /// created on demand. Never the app data dir itself: that holds the
    /// database and the swarm panes' credential copies.
    McpSandbox,
}

/// One MCP server manifest as parsed from JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerManifest {
    /// Stable id used as the row PK in `servers` and the
    /// `mcp:install(id)` argument.
    pub id: String,
    pub name: String,
    pub by: String,
    pub description: String,

    /// Spawn recipe for `mcp:install` and `mcp:callTool`. Required: a
    /// server Neuron cannot launch is not bundled.
    pub spawn: ManifestSpawn,

    /// OS-keychain entry name to look up before spawning. The exact
    /// keyring lookup is deferred to Week-3 when WP-W2-04's API-key
    /// surface lands; for now, presence is enough for the WP body's
    /// "surface a clear error if missing" requirement.
    #[serde(default)]
    pub requires_secret: Option<String>,

    /// Source of the default root path argument when the spawn template
    /// contains a `__ROOT__` placeholder. Only Filesystem uses this.
    #[serde(default)]
    pub default_root_kind: Option<DefaultRootKind>,
}

/// All bundled manifests in catalog order. Seeded by
/// [`crate::db::seed_mcp_servers`] at startup and looked up by
/// [`crate::mcp::registry`] at install/call time.
///
/// The order matters for the seed-list smoke test: it asserts ids in
/// this exact order.
pub const ALL_MANIFESTS_JSON: &[(&str, &str)] =
    &[("filesystem", include_str!("manifests/filesystem.json"))];

/// Outcome of parsing every bundled manifest exactly once. The
/// successfully-parsed entries land in `manifests`; per-file parse or
/// id-mismatch failures land in `failures` so a single bad JSON does
/// not brick the whole catalog at startup. Computed at most once per
/// process via [`parse_report`].
#[derive(Debug)]
pub struct ManifestParseReport {
    pub manifests: Vec<ServerManifest>,
    pub failures: Vec<ManifestParseFailure>,
}

#[derive(Debug)]
pub struct ManifestParseFailure {
    /// File-key (the first column of `ALL_MANIFESTS_JSON`).
    pub file_key: String,
    pub error: ManifestError,
}

static REPORT: OnceLock<ManifestParseReport> = OnceLock::new();

fn parse_uncached() -> ManifestParseReport {
    let mut manifests = Vec::with_capacity(ALL_MANIFESTS_JSON.len());
    let mut failures = Vec::new();
    for (id, raw) in ALL_MANIFESTS_JSON {
        match serde_json::from_str::<ServerManifest>(raw) {
            Err(e) => failures.push(ManifestParseFailure {
                file_key: (*id).to_string(),
                error: ManifestError::Parse((*id).to_string(), e.to_string()),
            }),
            Ok(m) if m.id != *id => failures.push(ManifestParseFailure {
                file_key: (*id).to_string(),
                error: ManifestError::IdMismatch {
                    file_key: (*id).to_string(),
                    manifest_id: m.id.clone(),
                },
            }),
            Ok(m) => manifests.push(m),
        }
    }
    ManifestParseReport {
        manifests,
        failures,
    }
}

/// Parse every bundled manifest exactly once and return a report
/// listing successes and per-file failures. Soft-loading callers
/// (e.g. [`crate::db::seed_mcp_servers`]) use this to log failures
/// while still seeding the survivors — a single bad JSON should not
/// abort app startup.
pub fn parse_report() -> &'static ManifestParseReport {
    REPORT.get_or_init(parse_uncached)
}

/// Strict load: fail on the first per-file error. Used by tests
/// asserting the bundled JSONs are pristine and by callers that want
/// "all-or-nothing" semantics.
pub fn load_all() -> Result<Vec<ServerManifest>, ManifestError> {
    let r = parse_report();
    if let Some(first) = r.failures.first() {
        return Err(first.error.clone());
    }
    Ok(r.manifests.clone())
}

/// Look up one manifest by id. Returns `Ok(None)` if the id is not in
/// the catalog or a failed-to-parse manifest's slot. Surface a
/// `ManifestError` only if **every** call would be unsafe (i.e. if
/// the requested id specifically failed to parse — callers looking up
/// an unrelated id can keep working).
pub fn get(id: &str) -> Result<Option<ServerManifest>, ManifestError> {
    let r = parse_report();
    if let Some(m) = r.manifests.iter().find(|m| m.id == id) {
        return Ok(Some(m.clone()));
    }
    if let Some(failure) = r.failures.iter().find(|f| f.file_key == id) {
        return Err(failure.error.clone());
    }
    Ok(None)
}

/// True when an npm spec names an EXACT version (`name@1.2.3`,
/// `@scope/name@2026.8.31`). A bare name, a dist-tag (`@latest`) or a
/// range (`^1`, `~1.2`, `>=1`, `1.x`, `*`) is rejected: `npx -y` would
/// fetch whatever is newest on every launch, so a compromised upstream
/// release would run on the user's machine unreviewed.
pub(crate) fn is_pinned_npm_spec(spec: &str) -> bool {
    // A scoped package starts with '@'; the version separator is the
    // LAST '@', which must come after the name.
    let Some(at) = spec.rfind('@').filter(|&i| i > 0) else {
        return false;
    };
    let version = &spec[at + 1..];
    // MAJOR.MINOR.PATCH (digits only), optionally followed by a
    // `-prerelease` / `+build` suffix, which may itself contain dots.
    let core_len = version.find(['-', '+']).unwrap_or(version.len());
    let (core, suffix) = version.split_at(core_len);
    let numeric = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    let core_ok = {
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3 && parts.iter().all(|p| numeric(p))
    };
    let suffix_ok = suffix.is_empty()
        || (suffix.len() > 1
            && suffix[1..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')));
    core_ok && suffix_ok
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest {0} failed to parse: {1}")]
    Parse(String, String),

    #[error(
        "manifest file key {file_key:?} does not match its inner id {manifest_id:?} \
         — fix the JSON or rename the file"
    )]
    IdMismatch {
        file_key: String,
        manifest_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Acceptance: every bundled JSON parses without errors and the
    /// file-key/id pair matches.
    #[test]
    fn load_all_parses_every_bundled_manifest() {
        let manifests = load_all().expect("manifests parse");
        assert_eq!(
            manifests.len(),
            ALL_MANIFESTS_JSON.len(),
            "every bundled manifest must surface in load_all()"
        );
        // The catalog order is asserted in db::seed_mcp_servers; here
        // we just confirm the ids are unique.
        let mut ids: Vec<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        let len = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len, "duplicate manifest id");
    }

    #[test]
    fn filesystem_manifest_is_rooted_in_its_own_sandbox() {
        let m = get("filesystem").expect("load").expect("present");
        assert_eq!(m.by, "Model Context Protocol");
        assert_eq!(m.default_root_kind, Some(DefaultRootKind::McpSandbox));
        let ManifestSpawn::Npx { package, args_template } = m.spawn;
        assert_eq!(package, "@modelcontextprotocol/server-filesystem@2026.8.31");
        assert_eq!(args_template, vec!["__ROOT__".to_string()]);
    }

    /// Only launchable servers are bundled (the design mock's
    /// catalog-only stubs and their invented stats are gone).
    #[test]
    fn the_catalog_lists_only_launchable_servers() {
        let ids: Vec<String> = load_all().expect("load").into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["filesystem".to_string()]);
        for (key, raw) in ALL_MANIFESTS_JSON {
            for field in ["\"installs\"", "\"rating\"", "\"featured\""] {
                assert!(!raw.contains(field), "manifest `{key}` still carries {field}");
            }
        }
    }

    #[test]
    fn every_npx_manifest_is_pinned_to_an_exact_version() {
        for m in load_all().expect("load") {
            let ManifestSpawn::Npx { package, .. } = &m.spawn;
            assert!(
                is_pinned_npm_spec(package),
                "manifest `{}` runs `{package}` unpinned — use name@x.y.z",
                m.id
            );
        }
    }

    #[test]
    fn npm_spec_pinning_rules() {
        for ok in [
            "left-pad@1.3.0",
            "@modelcontextprotocol/server-filesystem@2026.8.31",
            "@scope/pkg@1.0.0-beta.2",
            "pkg@1.2.3+build.5",
        ] {
            assert!(is_pinned_npm_spec(ok), "{ok}");
        }
        for bad in [
            "left-pad",
            "@modelcontextprotocol/server-filesystem",
            "pkg@latest",
            "pkg@^1.2.3",
            "pkg@~1.2",
            "pkg@>=1.0.0",
            "pkg@1.x",
            "pkg@*",
            "pkg@1.2",
            "@scope/pkg@",
        ] {
            assert!(!is_pinned_npm_spec(bad), "{bad}");
        }
    }

    /// A manifest without a launch recipe no longer parses: catalog-only
    /// entries cannot slip back into the bundle.
    #[test]
    fn a_manifest_without_a_spawn_recipe_is_rejected() {
        let stub = r#"{"id":"x","name":"X","by":"x","description":"x"}"#;
        assert!(serde_json::from_str::<ServerManifest>(stub).is_err());
    }

    #[test]
    fn unknown_id_returns_none() {
        assert!(get("not-a-server").expect("load").is_none());
    }
}

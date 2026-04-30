//! Bundled MCP server manifests.
//!
//! Each manifest describes one seeded server: the catalog metadata
//! (id, name, description, installs, rating, featured) and the spawn
//! recipe used by [`crate::mcp::registry`] when the user installs it.
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
    /// Tauri's per-app data dir, resolved at install time. The
    /// installer creates the dir if missing.
    AppDataDir,
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
    pub installs: i64,
    pub rating: f64,
    pub featured: bool,

    /// Spawn recipe for `mcp:install` and `mcp:callTool`. `None` means
    /// the manifest is a catalog-only stub — listing the server is
    /// fine but installing it returns
    /// [`crate::error::AppError::McpServerSpawnFailed`].
    #[serde(default)]
    pub spawn: Option<ManifestSpawn>,

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
pub const ALL_MANIFESTS_JSON: &[(&str, &str)] = &[
    ("filesystem", include_str!("manifests/filesystem.json")),
    ("github", include_str!("manifests/github.json")),
    ("postgres", include_str!("manifests/postgres.json")),
    ("browser", include_str!("manifests/browser.json")),
    ("slack", include_str!("manifests/slack.json")),
    ("vector-db", include_str!("manifests/vector-db.json")),
    // Catalog-only stubs added 2026-04-29 for mock-shape parity
    // (data.js#servers ships twelve rows). All have `spawn: null`,
    // so `mcp:install` against any of them returns
    // `McpServerSpawnFailed` until refactor.md G2 lands in Week 3.
    ("linear", include_str!("manifests/linear.json")),
    ("notion", include_str!("manifests/notion.json")),
    ("stripe", include_str!("manifests/stripe.json")),
    ("sentry", include_str!("manifests/sentry.json")),
    ("figma", include_str!("manifests/figma.json")),
    ("memory", include_str!("manifests/memory.json")),
];

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
    fn filesystem_manifest_has_app_data_root() {
        let m = get("filesystem").expect("load").expect("present");
        assert!(m.featured);
        assert_eq!(m.by, "Anthropic");
        assert_eq!(m.default_root_kind, Some(DefaultRootKind::AppDataDir));
        match m.spawn {
            Some(ManifestSpawn::Npx { package, args_template }) => {
                assert_eq!(package, "@modelcontextprotocol/server-filesystem");
                assert_eq!(args_template, vec!["__ROOT__".to_string()]);
            }
            _ => panic!("filesystem must spawn via npx"),
        }
    }

    #[test]
    fn github_manifest_requires_secret() {
        let m = get("github").expect("load").expect("present");
        assert_eq!(
            m.requires_secret.as_deref(),
            Some("GITHUB_PERSONAL_ACCESS_TOKEN"),
            "github MCP needs a PAT"
        );
        assert!(matches!(m.spawn, Some(ManifestSpawn::Npx { .. })));
    }

    #[test]
    fn stub_manifests_have_no_spawn() {
        // Browser is a pure catalog row in Week 2; install must surface
        // a clear error rather than silently flip the flag.
        let m = get("browser").expect("load").expect("present");
        assert!(m.spawn.is_none());
    }

    #[test]
    fn unknown_id_returns_none() {
        assert!(get("not-a-server").expect("load").is_none());
    }
}

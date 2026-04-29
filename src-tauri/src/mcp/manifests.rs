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
];

/// Parse every bundled manifest. Errors carry the offending id so a
/// future drift (e.g., a JSON typo) points to the right file in CI.
pub fn load_all() -> Result<Vec<ServerManifest>, ManifestError> {
    let mut out = Vec::with_capacity(ALL_MANIFESTS_JSON.len());
    for (id, raw) in ALL_MANIFESTS_JSON {
        let m: ServerManifest = serde_json::from_str(raw)
            .map_err(|e| ManifestError::Parse((*id).to_string(), e.to_string()))?;
        if m.id != *id {
            return Err(ManifestError::IdMismatch {
                file_key: (*id).to_string(),
                manifest_id: m.id,
            });
        }
        out.push(m);
    }
    Ok(out)
}

/// Look up one manifest by id.
pub fn get(id: &str) -> Result<Option<ServerManifest>, ManifestError> {
    Ok(load_all()?.into_iter().find(|m| m.id == id))
}

#[derive(Debug, thiserror::Error)]
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

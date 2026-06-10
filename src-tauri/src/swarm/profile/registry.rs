//! In-memory [`ProfileRegistry`]: loads `.md` agent profiles from the
//! bundled defaults and optional workspace overrides.
//!
//! Split out of the former monolithic `profile.rs` (WP-W3-11 §2). Two
//! source roots feed the registry, in order (workspace wins on `id`
//! collision per WP §2):
//!
//! 1. `<app_data_dir>/agents/*.md` — user-edited workspace overrides.
//!    Optional; missing dir is not an error.
//! 2. Bundled defaults embedded via `include_dir!` from
//!    `src-tauri/src/swarm/agents/*.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};

use crate::error::AppError;

use super::parser::parse_profile;
use super::types::{Profile, ProfileSource};

/// Bundled defaults — three persona files embedded at compile time.
/// `$CARGO_MANIFEST_DIR` is the `src-tauri/` dir; the path below
/// resolves to `src-tauri/src/swarm/agents/` containing
/// `scout.md`, `planner.md`, and `backend-builder.md`.
static BUNDLED_AGENTS: Dir<'_> =
    include_dir!("$CARGO_MANIFEST_DIR/src/swarm/agents");

/// In-memory directory of all loaded profiles. Workspace overrides
/// shadow bundled defaults sharing the same `id`.
pub struct ProfileRegistry {
    profiles: HashMap<String, Profile>,
    sources: HashMap<String, ProfileSource>,
}

impl ProfileRegistry {
    /// Load all profiles. The bundled set is always read; the
    /// workspace dir is read only when supplied and present
    /// (missing dir is not an error per WP §2).
    ///
    /// Workspace files override bundled ones with the same `id`; the
    /// override is logged at `tracing::debug!` level. Duplicate `id`s
    /// **within the same source** are an `InvalidInput` error.
    pub fn load_from(
        workspace_dir: Option<&Path>,
    ) -> Result<Self, AppError> {
        let mut profiles: HashMap<String, Profile> = HashMap::new();
        let mut sources: HashMap<String, ProfileSource> = HashMap::new();

        // 1. Bundled defaults (always available, embedded in binary).
        for file in BUNDLED_AGENTS.files() {
            // Skip non-`.md` files defensively — `include_dir!` only
            // grabs what's on disk, but a future contributor adding a
            // README or .gitkeep would otherwise blow up parsing.
            if file.path().extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let raw = std::str::from_utf8(file.contents()).map_err(|e| {
                AppError::InvalidInput(format!(
                    "{}: bundled profile is not utf-8: {e}",
                    file.path().display()
                ))
            })?;
            // Bundled profiles use the relative path from the embed
            // root as `source_path` for diagnostics; the prefix
            // `<bundled>/` makes the `bundled` vs. workspace
            // provenance unmistakable in error messages.
            let display = PathBuf::from("<bundled>")
                .join(file.path());
            let profile = parse_profile(raw, display.clone())?;
            // Duplicates *within* the bundled set are a developer bug
            // — fail loudly on startup so it's caught in CI before
            // shipping.
            if profiles.contains_key(&profile.id) {
                return Err(AppError::InvalidInput(format!(
                    "{}: duplicate bundled profile id `{}`",
                    display.display(),
                    profile.id
                )));
            }
            sources.insert(profile.id.clone(), ProfileSource::Bundled);
            profiles.insert(profile.id.clone(), profile);
        }

        // 2. Workspace overrides (optional, file-based).
        if let Some(dir) = workspace_dir {
            if dir.is_dir() {
                let mut seen_in_workspace: HashMap<String, PathBuf> =
                    HashMap::new();
                for entry in std::fs::read_dir(dir).map_err(|e| {
                    AppError::Internal(format!(
                        "read workspace agents dir {}: {e}",
                        dir.display()
                    ))
                })? {
                    let entry = entry.map_err(|e| {
                        AppError::Internal(format!(
                            "iter workspace agents dir {}: {e}",
                            dir.display()
                        ))
                    })?;
                    let path = entry.path();
                    if path.extension().map(|e| e != "md").unwrap_or(true) {
                        continue;
                    }
                    let raw = std::fs::read_to_string(&path).map_err(|e| {
                        AppError::Internal(format!(
                            "read workspace profile {}: {e}",
                            path.display()
                        ))
                    })?;
                    let profile = parse_profile(&raw, path.clone())?;
                    if let Some(prior) = seen_in_workspace.get(&profile.id) {
                        return Err(AppError::InvalidInput(format!(
                            "duplicate workspace profile id `{}` \
                             (first seen at {}, also at {})",
                            profile.id,
                            prior.display(),
                            path.display()
                        )));
                    }
                    seen_in_workspace
                        .insert(profile.id.clone(), path.clone());
                    if profiles.contains_key(&profile.id) {
                        tracing::debug!(
                            id = %profile.id,
                            path = %path.display(),
                            "workspace profile shadows bundled default"
                        );
                    }
                    sources
                        .insert(profile.id.clone(), ProfileSource::Workspace);
                    profiles.insert(profile.id.clone(), profile);
                }
            }
        }

        Ok(Self { profiles, sources })
    }

    /// Look up a profile by id. Returns `None` if neither source
    /// supplied one with this id.
    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.profiles.get(id)
    }

    /// Source provenance for a given id. `None` mirrors `get`'s miss.
    pub fn source(&self, id: &str) -> Option<ProfileSource> {
        self.sources.get(id).copied()
    }

    /// Every profile in the registry. Iteration order is unspecified;
    /// callers that need a stable order sort by `id` themselves.
    pub fn list(&self) -> Vec<&Profile> {
        self.profiles.values().collect()
    }

    pub fn profile_count(&self) -> usize { self.profiles.len() }

    /// Terminal-Hierarchy Swarm variant. Loads from
    /// `src/swarm/agents/term/*.md` (bundled via include_dir!) instead
    /// of the top-level W5 personas, with optional workspace overrides
    /// from `<workspace_dir>/term/*.md` when supplied.
    ///
    /// Identical parser, identical Profile shape — only the source root
    /// differs. Keeps the W5 mailbox-bus personas (carrying JSON
    /// OUTPUT CONTRACTs) untouched while letting the terminal-mode UI
    /// load prose-only siblings.
    pub fn load_term(
        workspace_dir: Option<&Path>,
    ) -> Result<Self, AppError> {
        let mut profiles: HashMap<String, Profile> = HashMap::new();
        let mut sources: HashMap<String, ProfileSource> = HashMap::new();

        let Some(term_dir) = BUNDLED_AGENTS.get_dir("term") else {
            return Err(AppError::Internal(
                "bundled `term/` agent dir missing".into(),
            ));
        };
        for file in term_dir.files() {
            if file.path().extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            let raw = std::str::from_utf8(file.contents()).map_err(|e| {
                AppError::InvalidInput(format!(
                    "{}: bundled term profile is not utf-8: {e}",
                    file.path().display()
                ))
            })?;
            let display = PathBuf::from("<bundled>").join(file.path());
            let profile = parse_profile(raw, display.clone())?;
            if profiles.contains_key(&profile.id) {
                return Err(AppError::InvalidInput(format!(
                    "{}: duplicate bundled term profile id `{}`",
                    display.display(),
                    profile.id
                )));
            }
            sources.insert(profile.id.clone(), ProfileSource::Bundled);
            profiles.insert(profile.id.clone(), profile);
        }

        if let Some(dir) = workspace_dir {
            if dir.is_dir() {
                let mut seen_in_workspace: HashMap<String, PathBuf> =
                    HashMap::new();
                for entry in std::fs::read_dir(dir).map_err(|e| {
                    AppError::Internal(format!(
                        "read workspace term agents dir {}: {e}",
                        dir.display()
                    ))
                })? {
                    let entry = entry.map_err(|e| {
                        AppError::Internal(format!(
                            "iter workspace term agents dir {}: {e}",
                            dir.display()
                        ))
                    })?;
                    let path = entry.path();
                    if path.extension().map(|e| e != "md").unwrap_or(true) {
                        continue;
                    }
                    let raw = std::fs::read_to_string(&path).map_err(|e| {
                        AppError::Internal(format!(
                            "read workspace term profile {}: {e}",
                            path.display()
                        ))
                    })?;
                    let profile = parse_profile(&raw, path.clone())?;
                    if let Some(prior) = seen_in_workspace.get(&profile.id) {
                        return Err(AppError::InvalidInput(format!(
                            "duplicate workspace term profile id `{}` \
                             (first seen at {}, also at {})",
                            profile.id,
                            prior.display(),
                            path.display()
                        )));
                    }
                    seen_in_workspace
                        .insert(profile.id.clone(), path.clone());
                    sources
                        .insert(profile.id.clone(), ProfileSource::Workspace);
                    profiles.insert(profile.id.clone(), profile);
                }
            }
        }

        Ok(Self { profiles, sources })
    }
}

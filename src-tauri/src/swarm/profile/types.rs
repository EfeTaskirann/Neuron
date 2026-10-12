//! Profile data types: permission posture, the parsed [`Profile`]
//! record, and its source provenance.
//!
//! Split out of the former monolithic `profile.rs` (WP-W3-11 §2). The
//! frontmatter parser lives in [`super::parser`] and the registry
//! loader in [`super::registry`]; the public shape here is unchanged
//! and re-exported via `swarm::profile::{…}`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::error::AppError;

/// Permission posture handed to the spawned `claude` subprocess.
///
/// Phase 1 (this WP) treats the value as a binary gate inside
/// `binding::build_specialist_args`:
///
/// - `Plan` → `--permission-mode plan` (no `--dangerously-skip-permissions`).
/// - everything else → `--dangerously-skip-permissions` (so the
///   smoke command can run without a UI prompt).
///
/// W3-12 introduces a per-tool allow / deny mapping; until then the
/// richer `AcceptEdits` / `AcceptAll` distinction is metadata only.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type,
)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// Read-only / planning posture — `--permission-mode plan`.
    Plan,
    /// Auto-accept Edit / Write tool calls.
    AcceptEdits,
    /// Auto-accept everything including Bash. Phase 1 gate is the same
    /// as `AcceptEdits`; W3-12 splits the two.
    AcceptAll,
}

impl PermissionMode {
    /// Parse a frontmatter `permission_mode:` value. Accepts the three
    /// canonical kebab / camel forms. Errors as `InvalidInput`.
    pub(super) fn parse(value: &str, source: &Path) -> Result<Self, AppError> {
        match value.trim() {
            "plan" | "Plan" => Ok(Self::Plan),
            "acceptEdits" | "accept-edits" | "accept_edits" => {
                Ok(Self::AcceptEdits)
            }
            "acceptAll" | "accept-all" | "accept_all" => Ok(Self::AcceptAll),
            other => Err(AppError::InvalidInput(format!(
                "{}: unknown permission_mode `{other}`; \
                 expected `plan` | `acceptEdits` | `acceptAll`",
                source.display()
            ))),
        }
    }
}

/// Parsed agent profile. The `body` is the persona prompt passed via
/// `--append-system-prompt-file`; `source_path` is for diagnostics
/// only and never crosses the IPC boundary.
#[derive(Debug, Clone)]
pub struct Profile {
    pub id: String,
    pub version: String,
    pub role: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub permission_mode: PermissionMode,
    pub max_turns: u32,
    pub body: String,
    pub source_path: PathBuf,
}

/// `"bundled"` for profiles embedded via `include_dir!`,
/// `"workspace"` for files read from `<app_data_dir>/agents/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Bundled,
    Workspace,
}

impl ProfileSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Workspace => "workspace",
        }
    }
}

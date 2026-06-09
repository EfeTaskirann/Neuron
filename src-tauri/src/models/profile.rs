//! Swarm profile summary type (WP-W3-11 `swarm:profiles_list`).

use serde::{Deserialize, Serialize};
use specta::Type;

/// IPC-friendly subset of `crate::swarm::profile::Profile` returned by
/// `swarm:profiles_list`. Strips the persona `body` (potentially
/// kilobyte-class markdown) and the on-disk `source_path` so the
/// frontend listing surface is cheap to fetch and stays free of
/// host-filesystem leaks. The `source` discriminant lets the UI label
/// each row as bundled-default vs. user-edited workspace override.
///
/// The full `Profile` (incl. body) is loaded server-side on every
/// `swarm:test_invoke`; the list command is purely a directory.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub version: String,
    pub role: String,
    pub description: String,
    pub permission_mode: crate::swarm::profile::PermissionMode,
    pub max_turns: u32,
    pub allowed_tools: Vec<String>,
    /// `"bundled"` for profiles embedded via `include_dir!`,
    /// `"workspace"` for files dropped under
    /// `<app_data_dir>/agents/`.
    pub source: String,
}

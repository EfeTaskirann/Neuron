//! User + workspace composite types (`me:get`).

use serde::{Deserialize, Serialize};
use specta::Type;

/// User profile fields surfaced in the Sidebar avatar / settings.
/// Mock parity: `Neuron Design/app/data.js#user`.
/// Week 2 hardcoded; Week 3 sources from a settings table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub initials: String,
    pub name: String,
}

/// Active workspace metadata. `count` is the number of workflows
/// currently saved (denormalised from `SELECT COUNT(*) FROM workflows`).
/// Mock parity: `Neuron Design/app/data.js#workspace`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub name: String,
    pub count: i64,
}

/// Composite shape returned by `me:get`. Combines `data.user` and
/// `data.workspace` so the Sidebar mounts in one round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user: User,
    pub workspace: Workspace,
}

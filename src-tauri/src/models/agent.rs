//! Agent domain types (`agents` table + create/patch inputs).

use serde::{Deserialize, Serialize};
use specta::Type;

/// One row of `agents`. Mirrors `data.js#agents[]` exactly.
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub model: String,
    pub temp: f64,
    pub role: String,
}

/// Input shape for `agents:create`. `id` is generated server-side
/// (ULID), so the frontend supplies only the user-visible fields.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentCreateInput {
    pub name: String,
    pub model: String,
    pub temp: f64,
    pub role: String,
}

/// Input shape for `agents:update`. Every field is optional — only the
/// fields actually sent are written. `id` is the URL parameter, not a
/// patch field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentPatch {
    pub name: Option<String>,
    pub model: Option<String>,
    pub temp: Option<f64>,
    pub role: Option<String>,
}

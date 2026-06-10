//! Workflow graph domain types (`workflows`/`nodes`/`edges` + detail).

use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    /// Unix epoch seconds when the workflow was last saved. Mirrors
    /// the WP-W2-02 column.
    pub saved_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub workflow_id: String,
    /// Constrained at SQL level to `'llm'|'tool'|'logic'|'human'|'mcp'`.
    pub kind: String,
    pub x: i64,
    pub y: i64,
    pub title: String,
    pub meta: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub workflow_id: String,
    pub from_node: String,
    pub to_node: String,
    /// SQLite stores 0/1 INTEGER; sqlx decodes that to `bool`
    /// natively (`sqlx_sqlite::types::bool`).
    pub active: bool,
}

/// Composite payload for `workflows:get` — the workflow itself plus
/// its full node and edge list. Matches the WP signature
/// `→ { workflow: Workflow; nodes: Node[]; edges: Edge[] }`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDetail {
    pub workflow: Workflow,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

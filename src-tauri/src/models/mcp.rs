//! MCP server + tool domain types (`servers`/`server_tools` + call result).

use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub by: String,
    /// Mock key: `desc`.
    #[serde(rename = "desc")]
    pub description: String,
    pub installs: i64,
    pub rating: f64,
    pub featured: bool,
    pub installed: bool,
}

/// One row of `server_tools`. Materialised by [`crate::mcp::registry`]
/// during `mcp:install`; consumed by the agent runtime (WP-W2-04) and
/// surfaced to the frontend via `mcp:listTools`.
///
/// `input_schema_json` is stored as a TEXT column (raw JSON Schema)
/// so the frontend can hand it directly to a JSON-Schema validator
/// without re-encoding. The wire shape uses `inputSchemaJson` to make
/// the schema-vs-string distinction explicit.
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub input_schema_json: String,
}

/// One block of a `tools/call` response. Mirrors the MCP spec's
/// content array element. We expose `text` natively and pass any
/// other shape through as `other` so the UI can render unknown blocks
/// best-effort instead of failing the whole call.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Other,
}

/// Wire shape for `mcp:callTool` returns. Keeps a flat `{content,
/// isError}` object so the frontend can rely on a single deserializer
/// regardless of which tool was called.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

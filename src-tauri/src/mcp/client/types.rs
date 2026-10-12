//! MCP domain types — the `tools/list` / `tools/call` wire shapes plus
//! the pinned protocol version. Shared with `crate::models` via the
//! `client` module re-export.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// MCP spec version this client speaks. Pinned per the Charter
/// risk register; upgrading is an ADR-shaped decision.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// One entry in `tools/list`'s `tools[]` array. The `input_schema`
/// field carries the raw JSON Schema as a `serde_json::Value` so we
/// never lose information during parse → persist → emit round-trips.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `inputSchema` per MCP spec. We keep it as a raw `Value` and
    /// re-serialize to a `TEXT` column on the DB side.
    #[serde(default)]
    pub input_schema: Value,
}

/// Response shape for `tools/call`. The MCP spec wraps the tool's
/// output in a `content[]` array of `{type, text|...}` blocks plus an
/// optional `isError` flag. `content` is `#[serde(default)]` because
/// the spec allows it to be absent on side-effect-only tools (e.g.,
/// `write_file`-style returns of `{"isError":false}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolOutput {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub is_error: bool,
}

/// One element of a `tools/call` response's `content` array. We keep
/// the variant set conservative (just `text`) and pass through
/// everything else as raw JSON so the frontend can render unknown
/// types best-effort.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    /// Anything else the spec adds in future versions (image, resource,
    /// embedded structured data, …). We keep the raw JSON instead of
    /// failing the whole call.
    #[serde(other)]
    Other,
}

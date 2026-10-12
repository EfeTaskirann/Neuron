//! JSON-RPC 2.0 envelope structs — the NDJSON frames written to / read
//! from an MCP server's stdio. Internal to the `client` module: every
//! struct and field is `pub(super)` so the [`connection`](super::connection)
//! submodule and the codec tests can construct and read them, while the
//! envelopes stay off the module's public surface.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub(super) struct Request<'a> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: u64,
    pub(super) method: &'a str,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub(super) params: Value,
}

#[derive(Debug, Serialize)]
pub(super) struct Notification<'a> {
    pub(super) jsonrpc: &'static str,
    pub(super) method: &'a str,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub(super) params: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct Response {
    #[allow(dead_code)]
    pub(super) jsonrpc: Option<String>,
    /// Notifications have no `id`; responses do. We use this to
    /// disambiguate one-off pushes (e.g., a server's progress
    /// notification) from the correlated reply we are waiting for.
    #[serde(default)]
    pub(super) id: Option<u64>,
    #[serde(default)]
    pub(super) result: Option<Value>,
    #[serde(default)]
    pub(super) error: Option<RpcError>,
    /// Servers may send unsolicited notifications between our
    /// requests. We log and skip them.
    #[serde(default)]
    pub(super) method: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RpcError {
    #[allow(dead_code)]
    pub(super) code: i64,
    pub(super) message: String,
}

//! Minimal MCP client — newline-delimited JSON-RPC 2.0 over stdio.
//!
//! Pinned MCP protocol version: **`2024-11-05`** (the version current
//! at WP-W2-05's authorship). Bumps go through an ADR per the Charter
//! risk register.
//!
//! ## Wire format
//!
//! Each message is one UTF-8 JSON object terminated by `\n`. This
//! differs from the WP-W2-04 length-prefixed sidecar framing —
//! Anthropic's reference MCP servers emit NDJSON so we follow suit
//! rather than fork the spec.
//!
//! ## Methods implemented
//!
//! - `initialize`               (request)
//! - `notifications/initialized` (notification, no response expected)
//! - `tools/list`               (request)
//! - `tools/call`               (request)
//! - `ping`                     (request)
//!
//! Subscriptions / resources / prompts are out of scope for Week 2 per
//! the WP body. A future package can extend [`McpClient::request`]
//! with new method names without touching the transport.
//!
//! ## Module layout
//!
//! - [`types`]      — `tools/list` / `tools/call` wire shapes plus the
//!                    pinned [`MCP_PROTOCOL_VERSION`].
//! - [`rpc`]        — JSON-RPC 2.0 envelope structs (internal NDJSON
//!                    frames; fields are `pub(super)`).
//! - [`connection`] — the stateful [`McpClient`] — owns the child
//!                    process + stdio pipes and drives the request /
//!                    notification protocol.
//!
//! The public surface (`McpClient`, the domain types, and the version
//! constant) is re-exported here so consumers keep using
//! `crate::mcp::client::{…}` unchanged.

mod connection;
mod rpc;
mod types;

pub use connection::McpClient;
pub use types::{CallToolOutput, ContentBlock, ToolDescriptor, MCP_PROTOCOL_VERSION};

#[cfg(test)]
mod tests;

//! Model Context Protocol (MCP) integration.
//!
//! WP-W2-05 introduces the in-house MCP client + registry. The module
//! is split into:
//!
//! - [`client`]   — newline-delimited JSON-RPC 2.0 transport over stdio
//!                  for one MCP server child process. Implements
//!                  `initialize`, `notifications/initialized`,
//!                  `tools/list`, `tools/call`, and `ping` — the
//!                  Week-2 minimum per the WP body.
//! - [`registry`] — install/uninstall flow that links seeded server
//!                  manifests (`manifests/*.json`) to the persisted
//!                  `servers` / `server_tools` rows.
//! - [`manifests`] — JSON manifest loader. Manifests are bundled into
//!                  the binary via `include_str!` so the installer is
//!                  self-contained.
//!
//! Charter §"Tech stack" pins MCP integration to the Anthropic Rust
//! client; we ship an in-house implementation because the published
//! `mcp-rs` crate is still tracking spec churn at the time of WP-05.
//! Upgrading to a published crate later is a drop-in replacement at
//! `client::stdio_call` — the registry never sees JSON-RPC details.

pub mod client;
pub mod manifests;
pub mod registry;

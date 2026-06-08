//! The live MCP server connection — owns the child process and its
//! stdio pipes, and drives the request / notification protocol over the
//! NDJSON [`rpc`](super::rpc) envelopes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::error::AppError;
use crate::tuning::MCP_REQUEST_TIMEOUT;

use super::rpc::{Notification, Request, Response};
use super::types::{CallToolOutput, ToolDescriptor, MCP_PROTOCOL_VERSION};

/// One live MCP server connection. Owns the child process and its
/// stdio pipes; drops kill the child via `kill_on_drop(true)`.
///
/// Construct with [`McpClient::spawn`] and tear down with
/// [`McpClient::shutdown`] — the latter drops stdin so the server
/// sees EOF and exits cleanly. Codec-only unit tests use the
/// `tests::TestClient` shape rather than `McpClient` to avoid
/// spawning a real subprocess.
pub struct McpClient {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn a server process and perform the MCP `initialize` +
    /// `notifications/initialized` handshake. Returns a ready-to-use
    /// client.
    pub async fn spawn(program: &str, args: &[String], env: &HashMap<String, String>) -> Result<Self, AppError> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // Inherit so npm/npx noise lands in the dev console; we
            // only consume stdout for protocol traffic.
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // Windows: `npx` ships as `npx.cmd`; spawning bare `npx`
        // returns ENOENT under tokio's `Command`. The caller is
        // responsible for passing the right basename, but on Windows
        // we tag the create-flags to suppress the console window pop
        // that npm scripts sometimes show (`CREATE_NO_WINDOW`).
        // tokio's `Command::creation_flags` is gated to Windows already
        // (it re-exports the std method), so no trait import is needed.
        #[cfg(windows)]
        cmd.creation_flags(0x08000000);
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::McpServerSpawnFailed(format!("{program}: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::McpServerSpawnFailed("stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::McpServerSpawnFailed("stdout missing".into()))?;
        let mut client = Self {
            child: Some(child),
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            next_id: AtomicU64::new(1),
        };
        client.handshake().await?;
        Ok(client)
    }

    async fn handshake(&mut self) -> Result<(), AppError> {
        let init_params = json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                // We don't expose roots/sampling/etc to the server in
                // Week 2 — the registry only consumes tools.
                "roots": { "listChanged": false }
            },
            "clientInfo": {
                "name": "neuron",
                "version": env!("CARGO_PKG_VERSION")
            }
        });
        let init_result = self.request("initialize", init_params).await?;
        // MCP spec 2024-11-05 §4.1: the client SHOULD verify the server
        // reports a compatible protocolVersion. A drift can silently
        // change the wire shape (e.g., tools/list envelope), so we log
        // loudly when we see one we did not request. We do not reject
        // the connection — some upstream servers bump versions while
        // staying wire-compatible — but the eprintln gives an audit
        // trail so a future "tools/list returned unexpected shape" bug
        // is correlatable to the version it shipped against.
        let server_version = init_result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if server_version.is_empty() {
            return Err(AppError::McpProtocol(
                "initialize response missing protocolVersion".into(),
            ));
        }
        if server_version != MCP_PROTOCOL_VERSION {
            tracing::warn!(
                expected = %MCP_PROTOCOL_VERSION,
                got = %server_version,
                "MCP protocolVersion mismatch; continuing optimistically"
            );
        }
        // Per the spec, the client MUST send `notifications/initialized`
        // after a successful `initialize` response.
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    /// Send a JSON-RPC request and wait for its correlated response.
    /// Notifications received before the matched response are
    /// logged-and-skipped — the WP body explicitly excludes
    /// subscriptions, so we do not surface them upward.
    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        let line = serde_json::to_string(&req)?;
        self.write_line(&line).await?;
        // Read until we get a response with the matching id. Bail out
        // after `MCP_REQUEST_TIMEOUT` to avoid a stuck UI thread when
        // the server crashes mid-call.
        let deadline = tokio::time::Instant::now() + MCP_REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(AppError::McpProtocol(format!(
                    "timeout waiting for response to {method}"
                )));
            }
            let line = match tokio::time::timeout(remaining, self.read_line()).await {
                Ok(r) => r?,
                Err(_) => {
                    return Err(AppError::McpProtocol(format!(
                        "timeout waiting for response to {method}"
                    )))
                }
            };
            let Some(line) = line else {
                return Err(AppError::McpProtocol(
                    "server closed stdout before responding".into(),
                ));
            };
            // Some MCP servers (Node line-buffering, slow startup) emit
            // stray blank lines into stdout. `read_line` already trims
            // CRLF; an empty `line` here is a keepalive, not a frame —
            // skip and keep waiting for the real response.
            if line.is_empty() {
                continue;
            }
            let resp: Response = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    return Err(AppError::McpProtocol(format!(
                        "decode response: {e} (line: {})",
                        line.trim()
                    )))
                }
            };
            if let Some(other_method) = resp.method.as_deref() {
                // Unsolicited notification — log and keep reading.
                tracing::debug!(
                    method = %other_method,
                    line = %line.trim(),
                    "ignoring unsolicited MCP notification"
                );
                continue;
            }
            // Surface any error before checking id correlation: per
            // JSON-RPC 2.0 §5.1, error responses for malformed requests
            // (parse error, invalid request) carry `id: null`. Since
            // this client never overlaps requests on a single instance,
            // any error response in flight is for our most recent
            // request — even when its echoed id is missing.
            if let Some(err) = resp.error {
                return Err(AppError::McpProtocol(format!(
                    "{method}: {}",
                    err.message
                )));
            }
            if resp.id != Some(id) {
                // Response for a different request — should not happen
                // since we never overlap requests, but fail loudly
                // rather than miscorrelate.
                return Err(AppError::McpProtocol(format!(
                    "response id mismatch: expected {id}, got {:?}",
                    resp.id
                )));
            }
            return Ok(resp.result.unwrap_or(Value::Null));
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), AppError> {
        let n = Notification {
            jsonrpc: "2.0",
            method,
            params,
        };
        let line = serde_json::to_string(&n)?;
        self.write_line(&line).await
    }

    /// Convenience: typed `tools/list` call.
    pub async fn list_tools(&mut self) -> Result<Vec<ToolDescriptor>, AppError> {
        let result = self.request("tools/list", json!({})).await?;
        // MCP spec: result = { tools: [{ name, description, inputSchema }, ...] }
        let tools_value = result
            .get("tools")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let tools: Vec<ToolDescriptor> = serde_json::from_value(tools_value).map_err(|e| {
            AppError::McpProtocol(format!("decode tools/list response: {e}"))
        })?;
        Ok(tools)
    }

    /// Convenience: typed `tools/call`.
    pub async fn call_tool(
        &mut self,
        name: &str,
        args: Value,
    ) -> Result<CallToolOutput, AppError> {
        let params = json!({ "name": name, "arguments": args });
        let result = self.request("tools/call", params).await?;
        let out: CallToolOutput = serde_json::from_value(result).map_err(|e| {
            AppError::McpProtocol(format!("decode tools/call response: {e}"))
        })?;
        Ok(out)
    }

    /// Send a `ping` request. Used by the registry to keep a long-
    /// lived connection healthy if Week-3 ever pools sessions.
    #[allow(dead_code)]
    pub async fn ping(&mut self) -> Result<(), AppError> {
        let _ = self.request("ping", json!({})).await?;
        Ok(())
    }

    /// Drop stdin so the server sees EOF, then await the child or
    /// kill it after 1s if it refuses to exit.
    pub async fn shutdown(mut self) {
        // Closing stdin first signals EOF to the server.
        drop(self.stdin.take());
        if let Some(mut child) = self.child.take() {
            // Race the child against a short kill timer.
            let kill_after =
                tokio::time::sleep(std::time::Duration::from_millis(1000));
            tokio::pin!(kill_after);
            tokio::select! {
                _ = child.wait() => {}
                _ = &mut kill_after => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            }
        }
    }

    async fn write_line(&mut self, line: &str) -> Result<(), AppError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| AppError::McpProtocol("stdin closed".into()))?;
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AppError::McpProtocol(format!("write line: {e}")))?;
        // The MCP spec uses NDJSON — each JSON object terminated by
        // exactly one `\n`. Servers often line-buffer their reads, so a
        // missing trailing newline deadlocks the handshake.
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| AppError::McpProtocol(format!("write newline: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AppError::McpProtocol(format!("flush stdin: {e}")))?;
        Ok(())
    }

    /// Read one NDJSON line, trimming the trailing `\n`. Returns
    /// `Ok(None)` on a clean EOF (server closed without bytes).
    async fn read_line(&mut self) -> Result<Option<String>, AppError> {
        let mut buf = String::new();
        let n = self
            .stdout
            .read_line(&mut buf)
            .await
            .map_err(|e| AppError::McpProtocol(format!("read line: {e}")))?;
        if n == 0 {
            return Ok(None);
        }
        // Strip the trailing newline so json deser does not see it.
        let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
        if trimmed.is_empty() {
            // Some servers emit blank keepalive lines; ignore.
            return Ok(Some(String::new()));
        }
        Ok(Some(trimmed))
    }
}

//! Codec round-trip tests use an in-memory pair of `tokio::io::
//! duplex` streams to stand in for the child's stdio. We re-
//! implement the read/write half of `McpClient` against the
//! generic streams so we never need a real subprocess.
//!
//! The integration test that actually spawns `npx
//! @modelcontextprotocol/server-filesystem` lives in
//! `crate::mcp::registry::tests::integration_*` and is
//! `#[ignore]`d so CI can opt in.

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as _BufReader};

use super::rpc::{Request, Response};
use super::types::{CallToolOutput, ContentBlock, ToolDescriptor, MCP_PROTOCOL_VERSION};

/// Test-only client over a generic `AsyncWrite`/`AsyncBufRead`
/// pair. Mirrors the production `McpClient` shape: one method per
/// public API, plus an internal `request` that correlates ids.
/// Keeping a separate type avoids exposing test internals on the
/// production `McpClient`.
struct TestClient<W, R>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncBufReadExt + Unpin,
{
    stdin: W,
    stdout: R,
    next_id: u64,
}

impl<W, R> TestClient<W, R>
where
    W: AsyncWriteExt + Unpin,
    R: AsyncBufReadExt + Unpin,
{
    async fn write_line(&mut self, s: &str) {
        self.stdin.write_all(s.as_bytes()).await.unwrap();
        self.stdin.write_all(b"\n").await.unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn read_line(&mut self) -> String {
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).await.unwrap();
        buf.trim_end_matches(['\r', '\n']).to_string()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = Request {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        self.write_line(&serde_json::to_string(&req).unwrap()).await;
        loop {
            let line = self.read_line().await;
            if line.is_empty() {
                continue;
            }
            let resp: Response = serde_json::from_str(&line).unwrap();
            if resp.method.is_some() {
                continue;
            }
            if resp.id == Some(id) {
                if let Some(err) = resp.error {
                    panic!("rpc error: {}", err.message);
                }
                return resp.result.unwrap_or(Value::Null);
            }
        }
    }
}

#[tokio::test]
async fn ndjson_request_response_round_trip() {
    // Two duplex streams: client_w → server_r, server_w → client_r.
    let (client_w, server_r) = tokio::io::duplex(1024);
    let (server_w, client_r) = tokio::io::duplex(1024);
    let mut client = TestClient {
        stdin: client_w,
        stdout: _BufReader::new(client_r),
        next_id: 1,
    };

    // Spawn a tiny "server" task that echoes one initialize request.
    tokio::spawn(async move {
        let mut reader = _BufReader::new(server_r);
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim_end()).unwrap();
        let id = req["id"].as_u64().unwrap();
        let resp = json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "serverInfo": { "name": "fake", "version": "0" }
            }
        });
        let mut server_w = server_w;
        let line = serde_json::to_string(&resp).unwrap();
        server_w.write_all(line.as_bytes()).await.unwrap();
        server_w.write_all(b"\n").await.unwrap();
        server_w.flush().await.unwrap();
    });

    let resp = client
        .request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name":"test","version":"0"}
            }),
        )
        .await;
    assert_eq!(resp["protocolVersion"], MCP_PROTOCOL_VERSION);
    assert_eq!(resp["serverInfo"]["name"], "fake");
}

#[tokio::test]
async fn unsolicited_notifications_are_skipped() {
    let (client_w, server_r) = tokio::io::duplex(1024);
    let (server_w, client_r) = tokio::io::duplex(1024);
    let mut client = TestClient {
        stdin: client_w,
        stdout: _BufReader::new(client_r),
        next_id: 1,
    };

    tokio::spawn(async move {
        let mut reader = _BufReader::new(server_r);
        let mut buf = String::new();
        reader.read_line(&mut buf).await.unwrap();
        let req: Value = serde_json::from_str(buf.trim_end()).unwrap();
        let id = req["id"].as_u64().unwrap();
        let mut server_w = server_w;
        // Push a notification first.
        let notif = json!({
            "jsonrpc":"2.0",
            "method":"notifications/progress",
            "params":{"progressToken":"x","progress":50}
        });
        server_w
            .write_all(serde_json::to_string(&notif).unwrap().as_bytes())
            .await
            .unwrap();
        server_w.write_all(b"\n").await.unwrap();
        // Then the actual response.
        let resp = json!({"jsonrpc":"2.0","id":id,"result":{"tools":[]}});
        server_w
            .write_all(serde_json::to_string(&resp).unwrap().as_bytes())
            .await
            .unwrap();
        server_w.write_all(b"\n").await.unwrap();
        server_w.flush().await.unwrap();
    });

    let resp = client.request("tools/list", json!({})).await;
    assert_eq!(resp["tools"], json!([]));
}

#[test]
fn tool_descriptor_decodes_minimal_shape() {
    let raw = json!({
        "name": "read_file",
        "description": "Reads a UTF-8 file",
        "inputSchema": {
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }
    });
    let t: ToolDescriptor = serde_json::from_value(raw).unwrap();
    assert_eq!(t.name, "read_file");
    assert!(!t.description.is_empty());
    assert_eq!(t.input_schema["type"], "object");
}

#[test]
fn call_tool_output_decodes_text_content() {
    let raw = json!({
        "content": [
            { "type": "text", "text": "hello" },
            { "type": "image", "data": "xxx", "mimeType": "image/png" }
        ],
        "isError": false
    });
    let out: CallToolOutput = serde_json::from_value(raw).unwrap();
    assert_eq!(out.content.len(), 2);
    match &out.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "hello"),
        _ => panic!("first should be text"),
    }
    // The image block decodes as `Other` because we only recognize
    // `text` natively in Week 2.
    assert!(matches!(out.content[1], ContentBlock::Other));
    assert!(!out.is_error);
}

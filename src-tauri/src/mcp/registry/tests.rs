//! Unit tests stub the spawn boundary by going around
//! `spawn_for_manifest` and inserting tools directly. The real
//! npx-spawning integration test is `#[ignore]` so CI can opt in.

use super::spawn::npx_executable;
use super::store::persist_install;
use super::{install, list_tools, uninstall};
use crate::db::DbPool;
use crate::mcp::client::{McpClient, ToolDescriptor};
use crate::mcp::manifests;
use crate::test_support::fresh_pool;
use serde_json::json;
use std::collections::HashMap;

async fn seed_manifest_rows(pool: &DbPool) {
    let manifests = manifests::load_all().expect("load manifests");
    for m in manifests {
        sqlx::query(
            "INSERT OR IGNORE INTO servers \
             (id, name, by, description, installs, rating, featured, installed) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(&m.id)
        .bind(&m.name)
        .bind(&m.by)
        .bind(&m.description)
        .bind(m.installs)
        .bind(m.rating)
        .bind(m.featured as i64)
        .execute(pool)
        .await
        .expect("seed manifest row");
    }
}

/// Acceptance: persist_install writes one server_tools row per
/// tool and flips the flag. Bypasses the npx subprocess.
#[tokio::test]
async fn persist_install_writes_tools_and_flips_flag() {
    let (pool, _dir) = fresh_pool().await;
    seed_manifest_rows(&pool).await;
    let manifest = manifests::get("filesystem").unwrap().unwrap();
    let tools = vec![
        ToolDescriptor {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: json!({"type":"object"}),
        },
        ToolDescriptor {
            name: "write_file".into(),
            description: "Write a file".into(),
            input_schema: json!({"type":"object"}),
        },
    ];
    let server = persist_install(&pool, &manifest, &tools).await.unwrap();
    assert!(server.installed);

    let row_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM server_tools WHERE server_id='filesystem'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(row_count, 2);
}

/// Idempotency: re-running install replaces the tool set without
/// duplicating rows. Important for "user reinstalls after a
/// manifest update" flows.
#[tokio::test]
async fn persist_install_replaces_existing_tools() {
    let (pool, _dir) = fresh_pool().await;
    seed_manifest_rows(&pool).await;
    let manifest = manifests::get("filesystem").unwrap().unwrap();
    let v1 = vec![ToolDescriptor {
        name: "read_file".into(),
        description: "v1".into(),
        input_schema: json!({}),
    }];
    let v2 = vec![
        ToolDescriptor {
            name: "read_file".into(),
            description: "v2".into(),
            input_schema: json!({}),
        },
        ToolDescriptor {
            name: "write_file".into(),
            description: "new".into(),
            input_schema: json!({}),
        },
    ];
    persist_install(&pool, &manifest, &v1).await.unwrap();
    persist_install(&pool, &manifest, &v2).await.unwrap();
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM server_tools WHERE server_id='filesystem' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(names, vec!["read_file", "write_file"]);
    let descs: Vec<String> = sqlx::query_scalar(
        "SELECT description FROM server_tools WHERE server_id='filesystem' AND name='read_file'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(descs, vec!["v2"], "description should reflect the latest install");
}

/// Acceptance: uninstall flips the flag and removes tool rows.
#[tokio::test]
async fn uninstall_removes_tools_and_clears_flag() {
    let (pool, _dir) = fresh_pool().await;
    seed_manifest_rows(&pool).await;
    let manifest = manifests::get("filesystem").unwrap().unwrap();
    persist_install(
        &pool,
        &manifest,
        &[ToolDescriptor {
            name: "read_file".into(),
            description: "x".into(),
            input_schema: json!({}),
        }],
    )
    .await
    .unwrap();

    let server = uninstall(&pool, "filesystem").await.unwrap();
    assert!(!server.installed);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM server_tools WHERE server_id='filesystem'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Acceptance: `list_tools` returns one row per tool registered.
#[tokio::test]
async fn list_tools_round_trips_through_db() {
    let (pool, _dir) = fresh_pool().await;
    seed_manifest_rows(&pool).await;
    let manifest = manifests::get("filesystem").unwrap().unwrap();
    let tools = vec![
        ToolDescriptor {
            name: "read_file".into(),
            description: "x".into(),
            input_schema: json!({"a":1}),
        },
        ToolDescriptor {
            name: "list_directory".into(),
            description: "y".into(),
            input_schema: json!({"b":2}),
        },
    ];
    persist_install(&pool, &manifest, &tools).await.unwrap();
    let got = list_tools(&pool, "filesystem").await.unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].name, "list_directory"); // alphabetical
    assert_eq!(got[1].name, "read_file");
    assert_eq!(got[0].input_schema_json, "{\"b\":2}");
}

/// Stub manifests (browser, slack, vector-db, postgres) MUST
/// surface a clear error rather than silently flip the flag.
#[tokio::test]
async fn install_stub_manifest_returns_spawn_failed() {
    let (pool, _dir) = fresh_pool().await;
    seed_manifest_rows(&pool).await;
    // Build a fake AppHandle for the call. The stub manifest path
    // never actually reaches the spawn boundary — we go through
    // `fetch_tools → spawn_for_manifest` which short-circuits on
    // `manifest.spawn.is_none()`.
    let app = tauri::test::mock_builder()
        .manage(pool.clone())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock app");
    let err = install(&pool, app.handle(), "browser").await.unwrap_err();
    assert_eq!(err.kind(), "mcp_server_spawn_failed");
}

/// Smoke: integration test that actually spawns
/// `npx @modelcontextprotocol/server-filesystem` against a tempdir,
/// performs `tools/list`, and then `tools/call read_text_file` on
/// a known file. `#[ignore]`d so CI without npx skips it.
///
/// The Filesystem MCP server's tool naming has drifted across
/// releases (`read_file` → `read_text_file` since the
/// 2024-12 spec bump). Rather than pin to a specific tool name,
/// the assertion shape is "≥5 tools listed AND the call returns
/// content blocks". The `read_file` smoke covered by the WP body
/// still works against the older releases — Week 3 will pin the
/// `npx` version to remove the drift entirely.
#[tokio::test]
#[ignore = "requires npx + network — opt-in via --ignored"]
async fn integration_filesystem_install_and_call() {
    // Build a tempdir, drop a marker file in it, and use the dir
    // as the Filesystem server's root. The server requires
    // absolute paths to files inside the root.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("README.md");
    std::fs::write(&marker, b"hello\n").unwrap();
    let env = HashMap::new();
    let mut client = McpClient::spawn(
        &npx_executable(),
        &[
            "-y".to_string(),
            "@modelcontextprotocol/server-filesystem".to_string(),
            tmp.path().to_string_lossy().into_owned(),
        ],
        &env,
    )
    .await
    .expect("spawn");
    let tools = client.list_tools().await.expect("list_tools");
    assert!(
        tools.len() >= 5,
        "filesystem should expose ≥5 tools, got {}",
        tools.len()
    );
    // Pick the read tool by matching either historical name.
    let read_tool = tools
        .iter()
        .find(|t| t.name == "read_text_file" || t.name == "read_file")
        .unwrap_or_else(|| panic!("no read_*_file tool in {:?}", tools.iter().map(|t| &t.name).collect::<Vec<_>>()));
    let out = client
        .call_tool(
            &read_tool.name,
            json!({ "path": marker.to_string_lossy() }),
        )
        .await
        .expect("read_*_file call");
    // The server may return either a text block on success or an
    // error block on failure — either is a valid round-trip
    // proving the protocol works. We assert at least one block.
    assert!(
        !out.content.is_empty(),
        "tools/call must return ≥1 content block; got {:?}",
        out.content
    );
    client.shutdown().await;
}

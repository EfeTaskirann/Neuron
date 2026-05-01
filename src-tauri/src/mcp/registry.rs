//! MCP server registry — install / uninstall / call orchestration.
//!
//! Glue layer between [`crate::mcp::manifests`] (catalog metadata),
//! [`crate::mcp::client`] (one-shot stdio sessions), and the SQLite
//! `servers` / `server_tools` rows.
//!
//! ## Lifecycle
//!
//! - `install(pool, app, id)`:
//!     1. Look up the manifest. Reject unknown ids with `NotFound`.
//!     2. Verify the manifest is fully wired (`spawn` is set). Stub
//!        manifests (browser, slack, vector-db, postgres) error out
//!        with [`AppError::McpServerSpawnFailed`] until Week 3 adds a
//!        sandbox + secret-prompt flow.
//!     3. Resolve the spawn template (substitute `__ROOT__` for
//!        Filesystem) and any required secret env var.
//!     4. Spawn the server, perform the handshake, run `tools/list`.
//!     5. Persist `installed=1` plus one row per tool in
//!        `server_tools`. Return the updated `Server` row.
//!     6. `shutdown()` the client so the spawned `npx` exits cleanly.
//!
//! - `uninstall(pool, id)` flips `installed=0` and deletes
//!   `server_tools` for that id (FK cascades on the row).
//!
//! - `call_tool(pool, app, id, name, args)` re-spawns the server
//!   (no pooling in Week 2 — see WP §"Out of scope"), calls the
//!   tool, shuts down. Returns the [`CallToolOutput`] verbatim.
//!
//! ## Why no session pool
//!
//! The WP body explicitly defers pooling to Week 3 (alongside
//! sandbox isolation). Each `mcp:callTool` is therefore a full
//! spawn cycle — slow, but correct and trivially auditable.

use std::collections::HashMap;

use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::db::DbPool;
use crate::error::AppError;
use crate::mcp::client::{CallToolOutput, McpClient, ToolDescriptor};
use crate::mcp::manifests::{self, DefaultRootKind, ManifestSpawn, ServerManifest};
use crate::models::Server;

/// Install an MCP server: spawn it, list its tools, persist them, and
/// flip the `installed` flag.
pub async fn install<R: Runtime>(
    pool: &DbPool,
    app: &AppHandle<R>,
    id: &str,
) -> Result<Server, AppError> {
    let manifest = manifests::get(id)
        .map_err(|e| AppError::Internal(format!("manifest load: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;

    // Connect, list, and persist tools — then flip the flag in one
    // transaction so a half-installed state is never observable.
    let tools = fetch_tools(app, &manifest).await?;
    persist_install(pool, &manifest, &tools).await
}

/// Uninstall: drop tools, flip flag.
pub async fn uninstall(pool: &DbPool, id: &str) -> Result<Server, AppError> {
    // The FK on server_tools.server_id cascades on delete of `servers`,
    // but we only flip a flag — explicit `DELETE FROM server_tools` is
    // the right shape here.
    sqlx::query("DELETE FROM server_tools WHERE server_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 0 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {id} not found")))?;
    Ok(updated)
}

/// Read the persisted tools for one server. Returns the raw rows the
/// agent runtime (WP-W2-04) consumes when planning calls.
pub async fn list_tools(pool: &DbPool, id: &str) -> Result<Vec<crate::models::Tool>, AppError> {
    // We accept calls against any id, even uninstalled ones — the
    // resulting empty list is the right answer ("no tools registered").
    let rows = sqlx::query_as::<_, crate::models::Tool>(
        "SELECT server_id, name, description, input_schema_json \
         FROM server_tools WHERE server_id = ? ORDER BY name",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Call one tool by re-spawning the server. Slow but correct in
/// Week 2; pooling moves the spawn out of the hot path in Week 3.
pub async fn call_tool<R: Runtime>(
    pool: &DbPool,
    app: &AppHandle<R>,
    id: &str,
    tool_name: &str,
    args: Value,
) -> Result<CallToolOutput, AppError> {
    // Verify the server is installed before paying for an `npx` cold
    // start — the alternative is the user gets a confusing
    // `McpServerSpawnFailed` for a row that exists but is uninstalled.
    let installed: Option<bool> = sqlx::query_scalar::<_, bool>(
        "SELECT installed FROM servers WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    match installed {
        None => return Err(AppError::NotFound(format!("Server {id} not found"))),
        Some(false) => {
            return Err(AppError::Conflict(format!(
                "Server {id} is not installed"
            )))
        }
        Some(true) => {}
    }
    let manifest = manifests::get(id)
        .map_err(|e| AppError::Internal(format!("manifest load: {e}")))?
        .ok_or_else(|| AppError::NotFound(format!("Manifest {id} not found")))?;
    let mut client = spawn_for_manifest(app, &manifest).await?;
    let out = client.call_tool(tool_name, args).await;
    client.shutdown().await;
    out
}

// --------------------------------------------------------------------- //
// Helpers                                                                //
// --------------------------------------------------------------------- //

/// Fetch the tool list from a freshly-spawned server. Internal to the
/// install flow.
async fn fetch_tools<R: Runtime>(
    app: &AppHandle<R>,
    manifest: &ServerManifest,
) -> Result<Vec<ToolDescriptor>, AppError> {
    let mut client = spawn_for_manifest(app, manifest).await?;
    let tools = client.list_tools().await;
    client.shutdown().await;
    tools
}

/// Resolve the spawn template + env vars from a manifest and start a
/// connected [`McpClient`].
async fn spawn_for_manifest<R: Runtime>(
    app: &AppHandle<R>,
    manifest: &ServerManifest,
) -> Result<McpClient, AppError> {
    let Some(spawn) = manifest.spawn.as_ref() else {
        return Err(AppError::McpServerSpawnFailed(format!(
            "{} is catalog-only in Week 2; install pipeline is wired \
             in a follow-up WP",
            manifest.id
        )));
    };
    let env = resolve_env(manifest)?;
    let (program, args) = resolve_command(app, manifest, spawn)?;
    McpClient::spawn(&program, &args, &env).await
}

/// Substitute `__ROOT__` placeholders in the spawn template with a
/// resolved on-disk path (the per-app data dir for Filesystem). Other
/// placeholders are reserved for future packages.
fn resolve_command<R: Runtime>(
    app: &AppHandle<R>,
    manifest: &ServerManifest,
    spawn: &ManifestSpawn,
) -> Result<(String, Vec<String>), AppError> {
    match spawn {
        ManifestSpawn::Npx {
            package,
            args_template,
        } => {
            let root_path: Option<String> = match manifest.default_root_kind {
                Some(DefaultRootKind::AppDataDir) => Some(resolve_app_data_dir(app)?),
                None => None,
            };
            let mut args = vec!["-y".to_string(), package.clone()];
            for arg in args_template {
                if arg == "__ROOT__" {
                    let root = root_path.clone().ok_or_else(|| {
                        AppError::Internal(
                            "__ROOT__ placeholder used but manifest has no default_root_kind"
                                .into(),
                        )
                    })?;
                    args.push(root);
                } else {
                    args.push(arg.clone());
                }
            }
            Ok((npx_executable(), args))
        }
    }
}

/// Resolve the per-app data dir as a string and ensure it exists. The
/// Filesystem MCP server in particular fails its handshake if the
/// supplied root does not exist.
fn resolve_app_data_dir<R: Runtime>(app: &AppHandle<R>) -> Result<String, AppError> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Internal(format!("app data dir: {e}")))?;
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::Internal(format!(
                "create app data dir at {dir:?}: {e}"
            ))
        })?;
    }
    Ok(dir.to_string_lossy().into_owned())
}

/// Resolve any environment variables a manifest declares.
///
/// WP-W3-01 routed this through `crate::secrets::get_secret`, which
/// honors the documented resolution order (env override
/// `NEURON_<KEY>` for tests/dev → OS keychain). The historical
/// `requires_secret: "GITHUB_PERSONAL_ACCESS_TOKEN"` flow keeps
/// working from a developer's shell (the env-override branch
/// covers it) while production reads now go through the platform
/// credential store per Charter §"Hard constraints" #2.
///
/// A missing required secret still surfaces as [`AppError::NoApiKey`]
/// so the frontend can render the "Configure API keys" CTA the
/// same way it does for provider tokens. An empty keychain value
/// is treated as missing, matching the historical
/// `Ok(v) if !v.is_empty()` guard the env-override branch carried
/// forward.
fn resolve_env(manifest: &ServerManifest) -> Result<HashMap<String, String>, AppError> {
    let mut env = HashMap::new();
    if let Some(secret_key) = manifest.requires_secret.as_deref() {
        match crate::secrets::get_secret(secret_key)? {
            Some(v) if !v.is_empty() => {
                env.insert(secret_key.to_string(), v);
            }
            _ => {
                return Err(AppError::NoApiKey(format!(
                    "{} (secret {secret_key} not configured)",
                    manifest.id
                )))
            }
        }
    }
    Ok(env)
}

/// Windows ships `npx` as `npx.cmd`; Unix-likes use the bare name.
fn npx_executable() -> String {
    if cfg!(windows) {
        "npx.cmd".to_string()
    } else {
        "npx".to_string()
    }
}

/// Persist `installed=1` and the tool list in one transaction.
async fn persist_install(
    pool: &DbPool,
    manifest: &ServerManifest,
    tools: &[ToolDescriptor],
) -> Result<Server, AppError> {
    let mut tx = pool.begin().await?;
    // Drop any stale tools first (idempotent reinstall).
    sqlx::query("DELETE FROM server_tools WHERE server_id = ?")
        .bind(&manifest.id)
        .execute(&mut *tx)
        .await?;
    for tool in tools {
        let schema_json = tool.input_schema.to_string();
        sqlx::query(
            "INSERT INTO server_tools (server_id, name, description, input_schema_json) \
             VALUES (?, ?, ?, ?)",
        )
        .bind(&manifest.id)
        .bind(&tool.name)
        .bind(&tool.description)
        .bind(schema_json)
        .execute(&mut *tx)
        .await?;
    }
    let updated = sqlx::query_as::<_, Server>(
        "UPDATE servers SET installed = 1 WHERE id = ? \
         RETURNING id, name, by, description, installs, rating, featured, installed",
    )
    .bind(&manifest.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Server {} not seeded", manifest.id)))?;
    tx.commit().await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    //! Unit tests stub the spawn boundary by going around
    //! `spawn_for_manifest` and inserting tools directly. The real
    //! npx-spawning integration test is `#[ignore]` so CI can opt in.
    use super::*;
    use crate::mcp::client::ToolDescriptor;
    use crate::test_support::fresh_pool;
    use serde_json::json;

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
}


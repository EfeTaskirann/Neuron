//! Spawn-side resolution for the MCP registry: turn a manifest into a
//! connected [`McpClient`].
//!
//! Owns command/arg templating (`__ROOT__` substitution), the
//! per-app-data-dir root resolution, secret-env resolution, and the
//! platform npx executable name. Everything here is internal to the
//! install / `call_tool` flow in [`super`].

use std::collections::HashMap;

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;
use crate::mcp::client::{McpClient, ToolDescriptor};
use crate::mcp::manifests::{
    is_pinned_npm_spec, DefaultRootKind, ManifestSpawn, ServerManifest,
};

/// Fetch the tool list from a freshly-spawned server. Internal to the
/// install flow.
pub(super) async fn fetch_tools<R: Runtime>(
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
pub(super) async fn spawn_for_manifest<R: Runtime>(
    app: &AppHandle<R>,
    manifest: &ServerManifest,
) -> Result<McpClient, AppError> {
    let env = resolve_env(manifest)?;
    let (program, args) = resolve_command(app, manifest, &manifest.spawn)?;
    McpClient::spawn(&program, &args, &env).await
}

/// Substitute `__ROOT__` placeholders in the spawn template with a
/// resolved on-disk path (the server's own sandbox folder for
/// Filesystem). Other placeholders are reserved for future packages.
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
            // Supply-chain guard: never let `npx -y` resolve "whatever is
            // newest" at launch time (also enforced by a manifest test).
            if !is_pinned_npm_spec(package) {
                return Err(AppError::McpServerSpawnFailed(format!(
                    "{} refuses to launch `{package}`: npm package must be \
                     pinned to an exact version (name@x.y.z)",
                    manifest.id
                )));
            }
            let root_path: Option<String> = match manifest.default_root_kind {
                Some(DefaultRootKind::McpSandbox) => Some(resolve_mcp_sandbox(app, &manifest.id)?),
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

/// Resolve `server_id`'s sandbox folder, `<app data>/mcp/<server_id>`,
/// as a string and ensure it exists. The Filesystem MCP server fails
/// its handshake if the supplied root does not exist. The app data dir
/// itself is never handed out: it holds the database and the swarm
/// panes' credential copies.
fn resolve_mcp_sandbox<R: Runtime>(app: &AppHandle<R>, server_id: &str) -> Result<String, AppError> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Internal(format!("app data dir: {e}")))?;
    let dir = mcp_sandbox_dir(&app_data, server_id);
    std::fs::create_dir_all(&dir).map_err(|e| {
        AppError::Internal(format!("create MCP sandbox at {dir:?}: {e}"))
    })?;
    Ok(dir.to_string_lossy().into_owned())
}

/// Pure: the sandbox folder of `server_id` under `app_data`.
pub(super) fn mcp_sandbox_dir(app_data: &std::path::Path, server_id: &str) -> std::path::PathBuf {
    app_data.join("mcp").join(server_id)
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
pub(super) fn npx_executable() -> String {
    if cfg!(windows) {
        "npx.cmd".to_string()
    } else {
        "npx".to_string()
    }
}

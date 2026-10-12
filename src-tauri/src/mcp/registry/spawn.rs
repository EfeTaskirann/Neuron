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
use crate::mcp::manifests::{DefaultRootKind, ManifestSpawn, ServerManifest};

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
pub(super) fn npx_executable() -> String {
    if cfg!(windows) {
        "npx.cmd".to_string()
    } else {
        "npx".to_string()
    }
}

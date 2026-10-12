//! Shared `claude` child spawn prelude.
//!
//! Split out of [`super::subprocess`] (DEEP refactor): the binary
//! resolution + persona tmp file + argv/env + `kill_on_drop` spawn +
//! pipe hand-off + stderr ring drain sequence used by BOTH the
//! one-shot [`super::subprocess::SubprocessTransport`] (Phase 1
//! transport) and the multi-turn `PersistentSession::spawn`
//! (WP-W4-01). Keeping it in its own module makes clear it is a shared
//! spawn primitive, not transport-internal; the driver-side read loop
//! and one-shot cleanup stay in [`super::subprocess`].

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use tauri::{AppHandle, Manager, Runtime};
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::swarm::binding::{
    build_specialist_args, resolve_claude_binary, subscription_env,
};
use crate::swarm::profile::Profile;

use super::ring::{RingBuffer, STDERR_RING_CAPACITY};

/// One spawned `claude` child with its pipes and stderr drain — the
/// shared spawn prelude of `SubprocessTransport::invoke` (one-shot)
/// and `PersistentSession::spawn` (multi-turn), which used to carry
/// near-verbatim ~70-line copies of this sequence.
pub(crate) struct SpawnedClaude {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: ChildStdout,
    /// 64 KiB tail ring fed by `stderr_task`; error paths read the
    /// tail for context.
    pub(crate) stderr_ring: Arc<Mutex<RingBuffer>>,
    /// Drain-task handle. One-shot callers let it finish on child
    /// death; the persistent session joins it on shutdown.
    pub(crate) stderr_task: tokio::task::JoinHandle<()>,
    /// Persona tmp file backing `--append-system-prompt-file`. The
    /// caller owns cleanup (guard or Drop impl).
    pub(crate) persona_tmp_path: PathBuf,
}

/// Spawn a `claude` child against `profile`: resolve the binary,
/// materialise the persona tmp file, build argv + env (including the
/// `STRIPPED_ENV_VARS` re-strip — `envs()` doesn't clear the
/// inherited slate), spawn with `kill_on_drop(true)`, take the three
/// pipes, and start the stderr ring drain. Every bail path removes
/// the persona tmp file; on success the caller owns it.
pub(crate) async fn spawn_claude_child<R: Runtime>(
    app: &AppHandle<R>,
    profile: &Profile,
) -> Result<SpawnedClaude, AppError> {
    // Resolve the binary. Missing → `ClaudeBinaryMissing` with the
    // resolution chain embedded in the message.
    let binary = resolve_claude_binary()?;

    // Persona tmp file under `<app_data_dir>/swarm/tmp/<ulid>.md` —
    // ULIDs so concurrent spawns don't collide and chronological
    // sorting helps post-mortem grepping.
    let tmp_path = write_persona_tmp(app, profile).await?;

    let env = subscription_env();
    let args = build_specialist_args(profile, &tmp_path);

    // Drop env vars that `subscription_env` wanted gone — keeping the
    // strip set centralized in `binding` makes future additions (e.g.
    // CLAUDE_CODE_OAUTH_TOKEN, which leaks from a parent Claude Code
    // shell and overrides credentials) apply uniformly across both
    // PTY-pane and subprocess spawn paths.
    let mut cmd = Command::new(&binary.path);
    cmd.envs(&env);
    for var in crate::swarm::binding::STRIPPED_ENV_VARS {
        cmd.env_remove(var);
    }
    let mut child = cmd
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            AppError::SwarmInvoke(format!(
                "spawn failed for `{}`: {e}",
                binary.path.display()
            ))
        })?;

    let take_pipe_err = |what: &str| {
        let _ = std::fs::remove_file(&tmp_path);
        AppError::SwarmInvoke(format!("child {what} pipe missing"))
    };
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| take_pipe_err("stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| take_pipe_err("stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| take_pipe_err("stderr"))?;

    // Stderr drain — ring-buffered so unbounded model spew can't OOM
    // the supervisor. The buffer is shared with the caller so error
    // paths can attach the most recent tail.
    let stderr_ring: Arc<Mutex<RingBuffer>> =
        Arc::new(Mutex::new(RingBuffer::new(STDERR_RING_CAPACITY)));
    let stderr_ring_for_task = Arc::clone(&stderr_ring);
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut buf = [0u8; 4096];
        loop {
            match tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let mut ring = stderr_ring_for_task.lock().await;
                    ring.append(&buf[..n]);
                }
                Err(_) => break,
            }
        }
    });

    Ok(SpawnedClaude {
        child,
        stdin,
        stdout,
        stderr_ring,
        stderr_task,
        persona_tmp_path: tmp_path,
    })
}

/// Materialise the persona body to disk so `--append-system-prompt-file`
/// has a path to read. Lives under `<app_data_dir>/swarm/tmp/<ulid>.md`
/// so a clean reinstall sweeps it up alongside the SQLite DB.
///
/// Pub-within-crate so `persistent_session.rs` reuses the same on-disk
/// convention (one tmp file per spawned session, same `<ulid>.md`
/// naming).
pub(crate) async fn write_persona_tmp<R: Runtime>(
    app: &AppHandle<R>,
    profile: &Profile,
) -> Result<PathBuf, AppError> {
    let base = app.path().app_data_dir().map_err(|e| {
        AppError::Internal(format!("app_data_dir resolution: {e}"))
    })?;
    let dir = base.join("swarm").join("tmp");
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| {
            AppError::Internal(format!(
                "create persona tmp dir {}: {e}",
                dir.display()
            ))
        })?;
    }
    let id = ulid::Ulid::new().to_string();
    let path = dir.join(format!("{id}.md"));
    std::fs::write(&path, profile.body.as_bytes()).map_err(|e| {
        AppError::Internal(format!(
            "write persona tmp file {}: {e}",
            path.display()
        ))
    })?;
    Ok(path)
}

//! The stateful `PersistentSession` — a single long-lived `claude`
//! child wired up to drive multi-turn stream-json conversations.
//! Spawn / invoke_turn / shutdown / Drop, plus the
//! `attach_stderr_tail` error-context helper.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Runtime};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, Notify};

use crate::error::AppError;
use crate::swarm::profile::Profile;
use crate::swarm::transport::{
    fmt_stderr_tail, spawn_claude_child, InvokeResult, RingBuffer,
    SpawnedClaude,
};

use super::event::TurnStreamEvent;
use super::read::{drain_post_cancel, read_until_result};

/// Graceful shutdown budget: drop stdin, wait this long for `claude`
/// to exit on its own. On expiry we kill the child explicitly.
const SHUTDOWN_GRACE_BUDGET: Duration = Duration::from_secs(2);

/// A single long-lived `claude` child wired up to drive multi-turn
/// stream-json conversations. See module docs.
pub struct PersistentSession {
    profile_id: String,
    persona_tmp_path: PathBuf,
    child: Child,
    /// `None` only after `shutdown()` has taken the pipe — `Option`
    /// purely so shutdown can move it out of `self` (`ChildStdin` has
    /// no `Default`; the previous dummy-process placeholder hack
    /// panicked on spawn failure inside the shutdown path).
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    stderr_ring: Arc<Mutex<RingBuffer>>,
    /// Stderr-drain task handle. Joined on `shutdown`. We hold the
    /// `JoinHandle` so the task is cleaned up deterministically rather
    /// than relying on the runtime's task scavenger.
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    turns_taken: u32,
}

impl PersistentSession {
    /// Spawn a fresh `claude` child against `profile`. Mirrors the
    /// argv contract of `SubprocessTransport::invoke` (same
    /// `build_specialist_args` + `subscription_env`), but retains
    /// the child + pipes for multi-turn use.
    pub async fn spawn<R: Runtime>(
        app: &AppHandle<R>,
        profile: &Profile,
    ) -> Result<Self, AppError> {
        // Shared spawn prelude with `SubprocessTransport::invoke`
        // (binary resolution, persona tmp file, argv/env incl. the
        // STRIPPED_ENV_VARS re-strip, kill_on_drop spawn, pipe
        // hand-off, stderr ring drain) — see
        // `transport::spawn_claude_child`. This session retains the
        // child + pipes for multi-turn use; the persona tmp file is
        // cleaned up by `shutdown()` / `Drop`.
        let SpawnedClaude {
            child,
            stdin,
            stdout,
            stderr_ring,
            stderr_task,
            persona_tmp_path,
        } = spawn_claude_child(app, profile).await?;

        Ok(Self {
            profile_id: profile.id.clone(),
            persona_tmp_path,
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            stderr_ring,
            stderr_task: Some(stderr_task),
            turns_taken: 0,
        })
    }

    /// Send `user_message` as the next turn, await the next `result`
    /// event, return the parsed `InvokeResult`. Child stays alive on
    /// return.
    ///
    /// `event_sink` (W4-03): if `Some`, streaming `AssistantText` and
    /// `ToolUse` events are forwarded to this channel as they arrive.
    /// `None` preserves the W4-01 silent-mode behavior (orchestrator
    /// decide IPC, unit tests).
    pub async fn invoke_turn(
        &mut self,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
        event_sink: Option<UnboundedSender<TurnStreamEvent>>,
    ) -> Result<InvokeResult, AppError> {
        // 1. Frame + write the user message. Stdin is NOT closed —
        //    that would signal "no more turns" to claude.
        let line = serde_json::to_string(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": user_message,
            }
        }))?;
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            AppError::SwarmInvoke(
                "session stdin already closed by shutdown".into(),
            )
        })?;
        stdin.write_all(line.as_bytes()).await.map_err(|e| {
            AppError::SwarmInvoke(format!(
                "write user message to claude stdin failed: {e}"
            ))
        })?;
        stdin.write_all(b"\n").await.map_err(|e| {
            AppError::SwarmInvoke(format!(
                "write newline to claude stdin failed: {e}"
            ))
        })?;
        stdin.flush().await.map_err(|e| {
            AppError::SwarmInvoke(format!(
                "flush claude stdin failed: {e}"
            ))
        })?;

        // 2. Read until next `result` event. The cancel notify races
        //    the read loop; on cancel we drain best-effort to keep
        //    framing aligned for the next turn.
        let read_outcome = read_until_result(
            &mut self.stdout,
            timeout,
            Some(Arc::clone(&cancel)),
            event_sink.as_ref(),
        )
        .await;

        match read_outcome {
            Ok(result) => {
                self.turns_taken = self.turns_taken.saturating_add(1);
                Ok(result)
            }
            Err(AppError::Cancelled(_)) => {
                // Best-effort drain so leftover bytes don't poison
                // the next turn. Bounded by both byte budget AND wall
                // time so cancel returns promptly.
                let _ = drain_post_cancel(&mut self.stdout).await;
                self.turns_taken = self.turns_taken.saturating_add(1);
                Err(AppError::Cancelled(
                    "swarm turn cancelled by user".into(),
                ))
            }
            Err(other) => {
                // For other errors (timeout / SwarmInvoke / EOF) we
                // also bump the turn counter — the turn happened
                // even though it failed. Caller can decide whether to
                // shut down the session.
                self.turns_taken = self.turns_taken.saturating_add(1);
                let tail = self.stderr_ring.lock().await.tail_string(2_048);
                Err(attach_stderr_tail(other, &tail))
            }
        }
    }

    /// Number of `invoke_turn` calls observed against this session
    /// (success, error, and cancel all count). Read by W4-02 to
    /// decide when to respawn under the turn-cap policy.
    pub fn turns_taken(&self) -> u32 {
        self.turns_taken
    }

    /// The profile id this session was spawned against.
    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    /// Explicit shutdown. Drops stdin (signals EOF to claude), waits
    /// up to `SHUTDOWN_GRACE_BUDGET` for graceful exit, then kills
    /// the child. Removes the persona tmp file.
    ///
    /// Idempotent at the `Drop` level: if the caller forgets
    /// `shutdown()`, the `Drop` impl still removes the persona tmp
    /// file and `kill_on_drop(true)` reaps the child.
    pub async fn shutdown(mut self) -> Result<(), AppError> {
        // 1. Drop stdin. The destructor closes the pipe → claude
        //    sees EOF and exits its read loop.
        drop(self.stdin.take());

        // 2. Grace window. If claude exits cleanly within the budget
        //    we skip the explicit kill. On Windows the AV layer can
        //    add ~1s here; the 2s budget is generous.
        let exit_outcome = tokio::time::timeout(
            SHUTDOWN_GRACE_BUDGET,
            self.child.wait(),
        )
        .await;

        if exit_outcome.is_err() {
            // Timed out — kill explicitly. `kill().await` is fine even
            // if the child has already exited (returns Ok per tokio).
            let _ = self.child.kill().await;
            let _ = self.child.wait().await;
        }

        // 3. Best-effort: join the stderr-drain task so it doesn't
        //    leak. The task exits when stderr EOFs (which happens at
        //    child death).
        if let Some(handle) = self.stderr_task.take() {
            let _ = handle.await;
        }

        // 4. Unlink the persona tmp file. Ignore errors — leaving a
        //    stray file is cleanup-grade, not load-bearing.
        let _ = std::fs::remove_file(&self.persona_tmp_path);

        Ok(())
    }
}

impl Drop for PersistentSession {
    /// Best-effort cleanup if the caller forgot `shutdown()`. The
    /// child is reaped by `kill_on_drop(true)` set at spawn time;
    /// we only need to remove the persona tmp file here.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.persona_tmp_path);
    }
}

/// Attach the most recent stderr tail to a `SwarmInvoke` error
/// message. Other error variants pass through unchanged — `Timeout`
/// and `Cancelled` already carry sufficient context.
fn attach_stderr_tail(err: AppError, tail: &str) -> AppError {
    match err {
        AppError::SwarmInvoke(msg) => AppError::SwarmInvoke(format!(
            "{msg}{}",
            fmt_stderr_tail(tail)
        )),
        other => other,
    }
}

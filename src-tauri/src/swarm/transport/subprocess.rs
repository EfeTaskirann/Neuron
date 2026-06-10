//! `SubprocessTransport` — one-shot `claude` CLI invocation (WP-W3-11 §4).
//!
//! Split out of the former single-file `transport.rs` (DEEP refactor):
//! this is the spawn/drive side. The pure event types live in
//! [`super::event`], the stream-json line classifier in
//! [`super::classify`], and the stderr ring in [`super::ring`].
//!
//! Mirrors the supervisor pattern in `crate::sidecar::agent` (`Command`
//! / `BufReader` / `kill_on_drop`) but inverts the lifecycle: instead
//! of one long-running process driven by JSON-RPC frames, every
//! `invoke` spawns a per-call child, drives a single user message
//! through stream-json, awaits a `result` event, and tears the child
//! down. Cold-start cost is real (Phase 1 accepts it; W3-13 may pool
//! sessions for hot specialists).
//!
//! Stream-json contract used here:
//!
//! - **stdin**: one NDJSON line —
//!   `{"type":"user","message":{"role":"user","content":"<msg>"}}`.
//!   Stdin is closed (dropped) right after, so the child stops waiting
//!   for further turns.
//! - **stdout**: one JSON object per line. We branch on `type`:
//!   - `"system"` + subtype `"init"` → capture `session_id`.
//!   - `"assistant"` → append text deltas to the running buffer.
//!   - `"result"` + subtype `"success"` → final
//!     `assistant_text`/`total_cost_usd`/`turn_count`; **stop reading**
//!     and return.
//!   - `"result"` + subtype `"error"` → bail with
//!     `AppError::SwarmInvoke`.
//!   - everything else → ignored (forward-compat).
//! - **stderr**: drained to a 64 KiB ring buffer; the tail is surfaced
//!   in error messages on a non-`result` exit.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::swarm::binding::{
    build_specialist_args, resolve_claude_binary, subscription_env,
};
use crate::swarm::profile::Profile;

use super::classify::classify_event;
use super::event::{InvokeResult, StreamEvent};
use super::ring::{fmt_stderr_tail, RingBuffer, STDERR_RING_CAPACITY};

/// Abstraction over "spawn a one-shot specialist call and return
/// its `result` event". WP-W3-12a's FSM was generic over this
/// trait; W5-06 deleted the FSM, so the only surviving consumer
/// today is `swarm:test_invoke` (one-shot persona-test IPC) and
/// `swarm:orchestrator_decide`. The trait is kept since both
/// callers still want a clean seam between the production
/// subprocess driver and any future mock.
///
/// WP-W5-06 — the FSM-only `mock_transport` module (MockTransport +
/// MockResponse) was deleted alongside `coordinator::fsm`. Brain
/// tests use `ScriptedCoordinatorInvoker` (in `swarm::brain`) and
/// the dispatcher tests use `agent_dispatcher::tests`'s mocked
/// invoker — neither speaks the `Transport` trait directly.
pub trait Transport: Send + Sync {
    /// Spawn one specialist invoke and wait up to `timeout` for the
    /// `result` event. Implementations must clean up any spawned
    /// child if the future is dropped (`SubprocessTransport` relies
    /// on `kill_on_drop(true)` for this).
    fn invoke<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        profile: &Profile,
        user_message: &str,
        timeout: Duration,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send;
}

/// Phase 1 transport. Stateless type — every `invoke` spawns its own
/// child. W3-13 may add a pooled variant alongside.
pub struct SubprocessTransport;

impl SubprocessTransport {
    /// Build a stateless transport. Provided so call-sites that
    /// previously held no value (`SubprocessTransport::invoke(...)`
    /// was an inherent associated fn pre-W3-12a) have a canonical
    /// constructor when transitioning to the trait method.
    pub fn new() -> Self {
        Self
    }
}

impl Default for SubprocessTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for SubprocessTransport {
    /// Spawn `claude` with `profile`'s persona, send `user_message`
    /// once, wait up to `timeout` for the `result` event. The
    /// subprocess is killed on `Child::drop` via `kill_on_drop(true)`
    /// and explicitly via `child.kill().await` on the bail paths.
    async fn invoke<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        profile: &Profile,
        user_message: &str,
        timeout: Duration,
    ) -> Result<InvokeResult, AppError> {
        // 1. Resolve the binary. Missing → `ClaudeBinaryMissing` with
        //    the resolution chain embedded in the message.
        let binary = resolve_claude_binary()?;

        // 2. Write the persona to a tmp file under
        //    `<app_data_dir>/swarm/tmp/<ulid>.md`. We use ULIDs (already
        //    a workspace dep) so concurrent invokes don't collide and
        //    chronological sorting helps post-mortem grepping.
        //    The guard removes the file on EVERY exit path below
        //    (spawn failure, pipe loss, timeout, read error, no-result
        //    EOF, happy path) — timeout/error exits are ordinary here,
        //    so per-site cleanup calls would always miss one.
        let tmp_path = write_persona_tmp(app, profile).await?;
        let _tmp_guard = PersonaTmpGuard(tmp_path.clone());

        // 3. Spawn. `kill_on_drop` is the seatbelt — if anything below
        //    panics or returns early, the child gets SIGKILL/TerminateProcess.
        let env = subscription_env();
        let args = build_specialist_args(profile, &tmp_path);

        // Drop env vars that `subscription_env` wanted gone —
        // `envs(&env)` doesn't *clear* the inherited slate, so re-strip
        // by iterating over the canonical `STRIPPED_ENV_VARS` list.
        // Keeping the strip set centralized in `binding` makes future
        // additions (e.g. CLAUDE_CODE_OAUTH_TOKEN, which leaks from a
        // parent Claude Code shell and overrides credentials) apply
        // uniformly across both PTY-pane and one-shot subprocess
        // spawn paths.
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
                AppError::SwarmInvoke(format!(
                    "spawn failed for `{}`: {e}",
                    binary.path.display()
                ))
            })?;

        // 4. Hand-off pipes.
        let mut stdin = child.stdin.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stdin pipe missing".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stdout pipe missing".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stderr pipe missing".into())
        })?;

        // 5. Stderr drain — ring-buffered so unbounded model spew can't
        //    OOM the supervisor. The buffer is shared with the read
        //    loop so error paths can attach the most recent tail.
        let stderr_ring: Arc<Mutex<RingBuffer>> =
            Arc::new(Mutex::new(RingBuffer::new(STDERR_RING_CAPACITY)));
        let stderr_ring_for_task = Arc::clone(&stderr_ring);
        let stderr_handle = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut buf = [0u8; 4096];
            loop {
                match tokio::io::AsyncReadExt::read(&mut reader, &mut buf)
                    .await
                {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut ring = stderr_ring_for_task.lock().await;
                        ring.append(&buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        // 6. Send the single user-message NDJSON line and close stdin.
        //    `serde_json::to_string` does the escaping; we never
        //    hand-build the JSON string per WP §"Hard rules".
        let line = serde_json::to_string(&json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": user_message
            }
        }))?;
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            // Try to surface stderr context if the child crashed
            // immediately on launch (e.g. unauthenticated subscription).
            let _ = child.kill().await;
            let tail = stderr_ring.lock().await.tail_string(2_048);
            return Err(AppError::SwarmInvoke(format!(
                "write user message to claude stdin failed: {e}{}",
                fmt_stderr_tail(&tail)
            )));
        }
        if let Err(e) = stdin.write_all(b"\n").await {
            let _ = child.kill().await;
            let tail = stderr_ring.lock().await.tail_string(2_048);
            return Err(AppError::SwarmInvoke(format!(
                "write newline to claude stdin failed: {e}{}",
                fmt_stderr_tail(&tail)
            )));
        }
        // Drop stdin → EOF for the child's reader.
        drop(stdin);

        // 7. Stdout reader loop, wrapped in `tokio::time::timeout`.
        let mut accum = InvokeAccum::default();
        let read_loop = async {
            let mut reader = BufReader::new(stdout).lines();
            loop {
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let value: Value =
                            match serde_json::from_str(&line) {
                                Ok(v) => v,
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        line = %line,
                                        "swarm transport: stdout line is not JSON"
                                    );
                                    continue;
                                }
                            };
                        // W4-03: classify_event now returns Vec.
                        // For one-shot transport we don't surface
                        // ToolUse events (no event sink at this
                        // layer); they're silently consumed. The
                        // streaming side (PersistentSession + W4-03
                        // event channel) is where ToolUse matters.
                        for ev in classify_event(&value) {
                            match ev {
                                StreamEvent::SystemInit { session_id } => {
                                    accum.session_id = Some(session_id);
                                }
                                StreamEvent::AssistantDelta { text } => {
                                    accum.assistant_text.push_str(&text);
                                }
                                StreamEvent::ToolUse { .. } => {
                                    // One-shot path: no event sink,
                                    // tool_use is informational only.
                                }
                                StreamEvent::ResultSuccess {
                                    assistant_text,
                                    total_cost_usd,
                                    turn_count,
                                } => {
                                    accum.final_text = Some(assistant_text);
                                    accum.total_cost_usd = total_cost_usd;
                                    accum.turn_count = turn_count;
                                    accum.completed = true;
                                    return Ok::<(), AppError>(());
                                }
                                StreamEvent::ResultError { reason } => {
                                    return Err(AppError::SwarmInvoke(reason));
                                }
                                StreamEvent::Other => {}
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF before a `result` event — let the post-
                        // loop branch surface stderr context.
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(AppError::SwarmInvoke(format!(
                            "stdout read error: {e}"
                        )));
                    }
                }
            }
        };

        let read_outcome = tokio::time::timeout(timeout, read_loop).await;

        // 8. Error / timeout handling. We always try to kill the child
        //    explicitly so its exit is observable on the same poll
        //    rather than waiting for `Child::drop`.
        match read_outcome {
            Err(_elapsed) => {
                let _ = child.kill().await;
                let _ = stderr_handle.await;
                let tail = stderr_ring.lock().await.tail_string(2_048);
                return Err(AppError::Timeout(format!(
                    "claude subprocess did not produce a `result` event \
                     within {:?}{}",
                    timeout,
                    fmt_stderr_tail(&tail)
                )));
            }
            Ok(Err(e)) => {
                let _ = child.kill().await;
                let _ = stderr_handle.await;
                return Err(e);
            }
            Ok(Ok(())) => {
                // Either we got `result.success` or stdout EOF'd
                // without one — distinguished below.
            }
        }

        // 9. Wait for the child to finish (we already drained its
        //    stdout). On the happy path we expect exit 0; on a
        //    no-result EOF we surface stderr.
        let exit = child.wait().await.map_err(|e| {
            AppError::SwarmInvoke(format!("waiting for child failed: {e}"))
        })?;
        let _ = stderr_handle.await;

        if !accum.completed {
            let tail = stderr_ring.lock().await.tail_string(2_048);
            return Err(AppError::SwarmInvoke(format!(
                "claude subprocess exited (status={}) without a \
                 `result.success` event{}",
                exit.code().map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                fmt_stderr_tail(&tail)
            )));
        }

        // 10. Happy path: `_tmp_guard` removes the persona tmp file
        //     when it drops on return.
        Ok(InvokeResult {
            session_id: accum.session_id.unwrap_or_default(),
            assistant_text: accum
                .final_text
                .unwrap_or(accum.assistant_text),
            total_cost_usd: accum.total_cost_usd,
            turn_count: accum.turn_count,
        })
    }
}

#[derive(Default)]
struct InvokeAccum {
    session_id: Option<String>,
    /// Running concatenation of `assistant` deltas. The `result` event
    /// usually carries the canonical final text in
    /// `value["result"]`; we keep this around as a fallback when only
    /// streaming deltas arrive.
    assistant_text: String,
    final_text: Option<String>,
    total_cost_usd: f64,
    turn_count: u32,
    completed: bool,
}

/// RAII cleanup for the one-shot persona tmp file. Never disarmed —
/// the file is one-shot by construction (the persona is also embedded
/// in the running registry, so nothing is lost). Removal failures are
/// logged at debug level; a stray file is not load-bearing.
struct PersonaTmpGuard(std::path::PathBuf);

impl Drop for PersonaTmpGuard {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.0) {
            tracing::debug!(
                path = %self.0.display(),
                error = %e,
                "could not remove persona tmp file (non-fatal)"
            );
        }
    }
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

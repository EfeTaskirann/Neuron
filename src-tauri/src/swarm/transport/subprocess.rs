//! `SubprocessTransport` — one-shot `claude` CLI invocation (WP-W3-11 §4).
//!
//! Split out of the former single-file `transport.rs` (DEEP refactor):
//! this is the drive side — the `Transport` trait, `SubprocessTransport`,
//! and the per-call stdout read loop. The shared child spawn prelude
//! lives in [`super::spawn`], the pure event types in [`super::event`],
//! the stream-json line classifier in [`super::classify`], and the
//! stderr ring in [`super::ring`].
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

use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::error::AppError;
use crate::swarm::profile::Profile;

use super::classify::classify_event;
use super::event::{InvokeResult, StreamEvent};
use super::ring::fmt_stderr_tail;
use super::spawn::{spawn_claude_child, SpawnedClaude};

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
        // 1-5. Shared spawn prelude: binary resolution, persona tmp
        //    file, argv/env, spawn with kill_on_drop, pipe hand-off,
        //    stderr ring drain. The guard removes the persona file on
        //    EVERY exit path below (timeout, read error, no-result
        //    EOF, happy path) — timeout/error exits are ordinary here,
        //    so per-site cleanup calls would always miss one.
        let SpawnedClaude {
            mut child,
            mut stdin,
            stdout,
            stderr_ring,
            stderr_task: stderr_handle,
            persona_tmp_path: tmp_path,
        } = spawn_claude_child(app, profile).await?;
        let _tmp_guard = PersonaTmpGuard(tmp_path);

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

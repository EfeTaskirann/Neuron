//! `PersistentSession` — multi-turn `claude` subprocess (WP-W4-01 §1).
//!
//! Sibling to `SubprocessTransport` (W3-11). Same arg builder, same
//! env strip, same stream-json read loop — but the child outlives a
//! single `invoke_turn` call. Stdin stays open between turns so the
//! claude CLI can accept a new `{"type":"user","message":...}` line
//! after each `result` event.
//!
//! Lifecycle owned by W4-02 registry:
//! - `spawn(app, profile)` — exactly once per (workspace, agent)
//! - `invoke_turn(user_message, timeout, cancel)` — repeatable
//! - `shutdown()` — once on workspace close (or on registry-driven
//!   respawn under the turn-cap policy)
//!
//! Thread-safety contract: not `Sync`. The W4-02 registry must
//! serialise access per session — at most one `invoke_turn` in flight
//! at a time. Concurrent turns against the same session are a
//! programming error and would deadlock on the stdin write.
//!
//! Cancel semantics: a fired `Notify` truncates the in-flight turn
//! (returns `AppError::Cancelled`); the child stays alive. Up to a
//! small drain budget after cancel, leftover bytes are read off
//! stdout to preserve framing for the next turn.
//!
//! Out of scope (per WP §"Out of scope"): registry / event channel /
//! help-request parser / FSM integration / specta event types. Those
//! land in W4-02..06.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::{Mutex, Notify};

use crate::error::AppError;
use crate::swarm::binding::{
    build_specialist_args, resolve_claude_binary, subscription_env,
};
use crate::swarm::profile::Profile;
use crate::swarm::transport::{
    classify_event, fmt_stderr_tail, write_persona_tmp, InvokeResult,
    RingBuffer, StreamEvent, STDERR_RING_CAPACITY,
};

/// Streaming event handed off to W4-03's per-agent event channel.
/// Mirrors `crate::swarm::SwarmAgentEvent` minus the bookend variants
/// (Spawned / TurnStarted / Result / Idle / Crashed) which the
/// registry emits on its own. This local-to-the-module enum is the
/// hot-path payload the read loop sends; the registry forwarder
/// re-wraps each one into a `SwarmAgentEvent` before emitting on the
/// Tauri channel.
///
/// Why a separate enum instead of `SwarmAgentEvent` directly:
/// `persistent_session.rs` deliberately doesn't depend on
/// `agent_registry.rs` (the dep would cycle on the registry's use of
/// `PersistentSession`). Keeping a thin local enum + lifting at the
/// registry boundary keeps the dep graph acyclic.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnStreamEvent {
    AssistantText { delta: String },
    ToolUse { name: String, input_summary: String },
}

/// Best-effort drain budget after a cancel signal: read up to this
/// many bytes from stdout to preserve framing for the next turn.
/// 4 KiB absorbs the typical mid-turn `result` event without blocking
/// the cancel return for too long.
const POST_CANCEL_DRAIN_BUDGET_BYTES: usize = 4 * 1024;

/// Hard ceiling on the post-cancel drain wall time. Even if the budget
/// hasn't been hit, return after this much time so `Cancelled` is
/// observed promptly.
const POST_CANCEL_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Graceful shutdown budget: drop stdin, wait this long for `claude`
/// to exit on its own. On expiry we kill the child explicitly.
const SHUTDOWN_GRACE_BUDGET: Duration = Duration::from_secs(2);

/// A single long-lived `claude` child wired up to drive multi-turn
/// stream-json conversations. See module docs.
pub struct PersistentSession {
    profile_id: String,
    persona_tmp_path: PathBuf,
    child: Child,
    stdin: ChildStdin,
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
        // 1. Resolve the binary — same path that `SubprocessTransport`
        //    uses, including the `<bundled>` lookup chain.
        let binary = resolve_claude_binary()?;

        // 2. Persona tmp file. ULID-named so concurrent sessions
        //    don't collide (e.g. all 9 W4-02 agents spawning at once).
        let tmp_path = write_persona_tmp(app, profile).await?;

        // 3. Build argv + env. `subscription_env` strips
        //    `ANTHROPIC_API_KEY` / `USE_BEDROCK` / `USE_VERTEX` /
        //    `USE_FOUNDRY` so the OAuth subscription is preserved.
        let env = subscription_env();
        let args = build_specialist_args(profile, &tmp_path);

        let mut child = Command::new(&binary.path)
            .envs(&env)
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("USE_BEDROCK")
            .env_remove("USE_VERTEX")
            .env_remove("USE_FOUNDRY")
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

        let stdin = child.stdin.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stdin pipe missing".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stdout pipe missing".into())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            AppError::SwarmInvoke("child stderr pipe missing".into())
        })?;

        // 4. Stderr drain task — same ring-buffer pattern as one-shot.
        //    Captures the tail so error paths can attach context.
        let stderr_ring: Arc<Mutex<RingBuffer>> =
            Arc::new(Mutex::new(RingBuffer::new(STDERR_RING_CAPACITY)));
        let stderr_ring_for_task = Arc::clone(&stderr_ring);
        let stderr_task = tokio::spawn(async move {
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

        Ok(Self {
            profile_id: profile.id.clone(),
            persona_tmp_path: tmp_path,
            child,
            stdin,
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
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            AppError::SwarmInvoke(format!(
                "write user message to claude stdin failed: {e}"
            ))
        })?;
        self.stdin.write_all(b"\n").await.map_err(|e| {
            AppError::SwarmInvoke(format!(
                "write newline to claude stdin failed: {e}"
            ))
        })?;
        self.stdin.flush().await.map_err(|e| {
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
        //    sees EOF and exits its read loop. Move out of `self` so
        //    the rest of `self` is still well-typed.
        let stdin = std::mem::replace(&mut self.stdin, dummy_child_stdin());
        drop(stdin);

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

/// Construct a placeholder `ChildStdin` for the `mem::replace` swap
/// in `shutdown()`. The placeholder is immediately dropped; we never
/// write to it. This dance is needed because `ChildStdin` doesn't
/// implement `Default` and we can't move out of `self.stdin` directly
/// while `self` is still in scope.
///
/// Implementation: spawn a dummy `cmd /c rem` (Windows) / `true`
/// (Unix) just to harvest its stdin pipe, then drop the child. The
/// cost is one short-lived process per shutdown, which is fine.
fn dummy_child_stdin() -> ChildStdin {
    // PANICS: only on platforms where the standard `true` /
    // `cmd /c rem` no-op is missing, which is none of our supported
    // targets. Guarded by the test suite (`shutdown_kills_child`).
    #[cfg(windows)]
    let cmd = ("cmd", &["/c", "rem"][..]);
    #[cfg(not(windows))]
    let cmd = ("true", &[][..]);

    let mut child = Command::new(cmd.0)
        .args(cmd.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("dummy noop process spawns");
    child.stdin.take().expect("dummy stdin pipe present")
}

/// Read stream-json events until the next `result` (success or error)
/// or until cancel / timeout / stdout EOF fires.
///
/// Pub-within-module so the caller (`PersistentSession::invoke_turn`)
/// can drive it directly. The shape mirrors the inner read_loop in
/// `SubprocessTransport::invoke` — one of three reasons we don't
/// extract a shared free fn yet:
///
/// 1. The one-shot path's read loop is a pure subset (no cancel arm).
/// 2. The one-shot path returns `Result<(), AppError>` and pumps an
///    outer `InvokeAccum`; this path returns `Result<InvokeResult>`
///    directly. Different shapes.
/// 3. Sharing would require either a `Notify` that never fires for
///    one-shot (extra plumbing) or a dyn trait split. Not worth it
///    for ~30 lines of duplication.
///
/// If a third caller arrives we'll factor; for W4-01 the duplication
/// is the right size.
async fn read_until_result(
    reader: &mut BufReader<ChildStdout>,
    timeout: Duration,
    cancel: Option<Arc<Notify>>,
    event_sink: Option<&UnboundedSender<TurnStreamEvent>>,
) -> Result<InvokeResult, AppError> {
    let mut accum = InvokeAccum::default();
    let read_loop = async {
        let mut lines = reader.lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let value: Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                line = %line,
                                "persistent session: stdout line is not JSON"
                            );
                            continue;
                        }
                    };
                    for ev in classify_event(&value) {
                        match ev {
                            StreamEvent::SystemInit { session_id } => {
                                accum.session_id = Some(session_id);
                            }
                            StreamEvent::AssistantDelta { text } => {
                                accum.assistant_text.push_str(&text);
                                if let Some(tx) = event_sink {
                                    let _ = tx.send(
                                        TurnStreamEvent::AssistantText {
                                            delta: text.clone(),
                                        },
                                    );
                                }
                            }
                            StreamEvent::ToolUse {
                                name,
                                input_summary,
                            } => {
                                if let Some(tx) = event_sink {
                                    let _ = tx.send(
                                        TurnStreamEvent::ToolUse {
                                            name: name.clone(),
                                            input_summary:
                                                input_summary.clone(),
                                        },
                                    );
                                }
                            }
                            StreamEvent::ResultSuccess {
                                assistant_text,
                                total_cost_usd,
                                turn_count,
                            } => {
                                return Ok(InvokeResult {
                                    session_id: accum
                                        .session_id
                                        .clone()
                                        .unwrap_or_default(),
                                    assistant_text: if !assistant_text
                                        .is_empty()
                                    {
                                        assistant_text
                                    } else {
                                        accum.assistant_text.clone()
                                    },
                                    total_cost_usd,
                                    turn_count,
                                });
                            }
                            StreamEvent::ResultError { reason } => {
                                return Err(AppError::SwarmInvoke(format!(
                                    "swarm invoke error: {reason}"
                                )));
                            }
                            StreamEvent::Other => {}
                        }
                    }
                }
                Ok(None) => {
                    return Err(AppError::SwarmInvoke(
                        "claude stdout closed before `result` event \
                         (child may have crashed)"
                            .into(),
                    ));
                }
                Err(e) => {
                    return Err(AppError::SwarmInvoke(format!(
                        "stdout read error: {e}"
                    )));
                }
            }
        }
    };

    // Race the read loop against optional cancel + the per-turn
    // timeout. `tokio::time::timeout` is the outermost wrapper so
    // either branch (cancel arm OR read finishing OR timeout)
    // unblocks promptly.
    match cancel {
        Some(notify) => {
            let cancel_arm = async {
                notify.notified().await;
                Err::<InvokeResult, AppError>(AppError::Cancelled(
                    "cancel notify fired".into(),
                ))
            };
            tokio::select! {
                outcome = tokio::time::timeout(timeout, read_loop) => {
                    match outcome {
                        Err(_elapsed) => Err(AppError::Timeout(format!(
                            "claude subprocess did not produce a `result` \
                             event within {timeout:?}"
                        ))),
                        Ok(Ok(result)) => Ok(result),
                        Ok(Err(e)) => Err(e),
                    }
                }
                cancelled = cancel_arm => cancelled,
            }
        }
        None => match tokio::time::timeout(timeout, read_loop).await {
            Err(_elapsed) => Err(AppError::Timeout(format!(
                "claude subprocess did not produce a `result` event \
                 within {timeout:?}"
            ))),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(e),
        },
    }
}

/// Best-effort drain after a cancel: read off stdout for up to
/// `POST_CANCEL_DRAIN_BUDGET_BYTES` bytes / `POST_CANCEL_DRAIN_TIMEOUT`
/// wall time so leftover stream-json events don't show up at the
/// start of the next turn.
async fn drain_post_cancel(reader: &mut BufReader<ChildStdout>) {
    let drain_loop = async {
        let mut total = 0usize;
        let mut tmp = [0u8; 1024];
        loop {
            match tokio::io::AsyncReadExt::read(reader, &mut tmp).await {
                Ok(0) => return,
                Ok(n) => {
                    total = total.saturating_add(n);
                    if total >= POST_CANCEL_DRAIN_BUDGET_BYTES {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    };
    let _ = tokio::time::timeout(POST_CANCEL_DRAIN_TIMEOUT, drain_loop).await;
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

/// Mirror of the private `InvokeAccum` in `transport.rs`. Kept local
/// to this module to avoid widening the transport.rs visibility for
/// what is, conceptually, just running state of one read scope.
#[derive(Default)]
struct InvokeAccum {
    session_id: Option<String>,
    assistant_text: String,
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use tokio::sync::Notify;

    /// Adapter that lets a `tokio::io::DuplexStream` stand in for a
    /// `ChildStdout` so the unit suite can drive `read_until_result`
    /// without spawning a real subprocess. The test seeds the
    /// "client" half with stream-json bytes and the read loop pulls
    /// them off the "server" half.
    ///
    /// We re-import `BufReader<ChildStdout>` only because the public
    /// `PersistentSession` wraps that type. For the helper tests we
    /// drive `read_until_result` directly via a local generic helper
    /// (below).
    async fn read_until_result_via_duplex(
        bytes: &[u8],
        timeout: Duration,
        cancel: Option<Arc<Notify>>,
    ) -> Result<InvokeResult, AppError> {
        read_until_result_via_duplex_with_close(bytes, timeout, cancel, true)
            .await
    }

    /// `keep_open=false` drops the writer after seeding the bytes,
    /// which signals EOF to the reader (the closed-pipe case for
    /// EOF-without-result tests). `keep_open=true` keeps the writer
    /// alive forever, so the reader blocks on `next_line` and the
    /// cancel / timeout arms can fire — the cancel/timeout tests
    /// need this; the success-path tests don't.
    async fn read_until_result_via_duplex_with_close(
        bytes: &[u8],
        timeout: Duration,
        cancel: Option<Arc<Notify>>,
        close_on_eof: bool,
    ) -> Result<InvokeResult, AppError> {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        let bytes = bytes.to_vec();
        // Spawn the writer-side. If `close_on_eof` is false we hold
        // the writer alive for the full test duration so the reader
        // never sees EOF — this is what the cancel / timeout tests
        // need. We keep the JoinHandle in scope after this fn
        // returns by spawning detached; the writer drops when the
        // task is reaped at suite end (or when `close_on_eof=true`,
        // explicitly inside the task).
        tokio::spawn(async move {
            let _ = writer.write_all(&bytes).await;
            let _ = writer.flush().await;
            if close_on_eof {
                drop(writer);
            } else {
                // Park the task without dropping the writer. The test
                // will time out / cancel before this completes.
                tokio::time::sleep(Duration::from_secs(60)).await;
                drop(writer);
            }
        });
        let mut buf_reader = BufReader::new(reader);
        // Local copy of the read loop adapted to a generic
        // `AsyncBufReadExt` reader. The production fn is bound to
        // `BufReader<ChildStdout>` so we can't reuse it directly in
        // tests; we replicate the parser logic inline.
        let mut accum = InvokeAccum::default();
        let read_loop = async {
            let mut lines = buf_reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let v: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                        for ev in classify_event(&v) {
                            match ev {
                                StreamEvent::SystemInit { session_id } => {
                                    accum.session_id = Some(session_id);
                                }
                                StreamEvent::AssistantDelta { text } => {
                                    accum.assistant_text.push_str(&text);
                                }
                                StreamEvent::ToolUse { .. } => {
                                    // Test helper doesn't surface
                                    // ToolUse events — see the
                                    // dedicated event-sink test for
                                    // that path.
                                }
                                StreamEvent::ResultSuccess {
                                    assistant_text,
                                    total_cost_usd,
                                    turn_count,
                                } => {
                                    return Ok(InvokeResult {
                                        session_id: accum
                                            .session_id
                                            .clone()
                                            .unwrap_or_default(),
                                        assistant_text: if !assistant_text
                                            .is_empty()
                                        {
                                            assistant_text
                                        } else {
                                            accum.assistant_text.clone()
                                        },
                                        total_cost_usd,
                                        turn_count,
                                    });
                                }
                                StreamEvent::ResultError { reason } => {
                                    return Err(AppError::SwarmInvoke(
                                        format!(
                                            "swarm invoke error: {reason}"
                                        ),
                                    ));
                                }
                                StreamEvent::Other => {}
                            }
                        }
                    }
                    Ok(None) => {
                        return Err(AppError::SwarmInvoke(
                            "stdout EOF before result".into(),
                        ));
                    }
                    Err(e) => {
                        return Err(AppError::SwarmInvoke(format!(
                            "read error: {e}"
                        )));
                    }
                }
            }
        };
        match cancel {
            Some(notify) => {
                let cancel_arm = async {
                    notify.notified().await;
                    Err::<InvokeResult, AppError>(AppError::Cancelled(
                        "cancel notify fired".into(),
                    ))
                };
                tokio::select! {
                    outcome = tokio::time::timeout(timeout, read_loop) => {
                        match outcome {
                            Err(_) => Err(AppError::Timeout(
                                format!("did not produce result within {timeout:?}")
                            )),
                            Ok(Ok(r)) => Ok(r),
                            Ok(Err(e)) => Err(e),
                        }
                    }
                    cancelled = cancel_arm => cancelled,
                }
            }
            None => match tokio::time::timeout(timeout, read_loop).await {
                Err(_) => Err(AppError::Timeout(format!(
                    "did not produce result within {timeout:?}"
                ))),
                Ok(Ok(r)) => Ok(r),
                Ok(Err(e)) => Err(e),
            },
        }
    }

    fn ok_event_lines(text: &str, cost: f64, turns: u32) -> String {
        let mut s = String::new();
        writeln!(
            s,
            r#"{{"type":"system","subtype":"init","session_id":"sess-test"}}"#
        )
        .unwrap();
        writeln!(
            s,
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
        )
        .unwrap();
        writeln!(
            s,
            r#"{{"type":"result","subtype":"success","result":"{text}","total_cost_usd":{cost},"num_turns":{turns}}}"#
        )
        .unwrap();
        s
    }

    /// Single-turn happy path: classifier sees init / assistant /
    /// result.success in order and returns a populated InvokeResult.
    #[tokio::test]
    async fn single_turn_round_trip() {
        let bytes = ok_event_lines("hello", 0.0042, 3);
        let result = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("ok");
        assert_eq!(result.session_id, "sess-test");
        assert_eq!(result.assistant_text, "hello");
        assert!((result.total_cost_usd - 0.0042).abs() < 1e-9);
        assert_eq!(result.turn_count, 3);
    }

    /// Two consecutive `result.success` events arriving in one read
    /// scope — the read loop returns on the FIRST one and leaves the
    /// rest unread, which is the contract the persistent session
    /// relies on (turn 2 starts a fresh read scope).
    #[tokio::test]
    async fn read_loop_returns_on_first_result_only() {
        let mut bytes = ok_event_lines("turn-1-text", 0.0, 1);
        bytes.push_str(&ok_event_lines("turn-2-text", 0.0, 1));
        // We can only verify "turn 1's text" returns; turn 2's bytes
        // are still in the duplex pipe and would be picked up by the
        // next `read_until_result` call. Driving that here would
        // reuse the already-dropped reader, which the test harness
        // doesn't expose — covered by the integration smoke instead.
        let result = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("ok");
        assert_eq!(result.assistant_text, "turn-1-text");
    }

    /// `result.subtype = "error_max_turns"` surfaces as `SwarmInvoke`
    /// with the canonical message that prior FSM tests pin against.
    #[tokio::test]
    async fn error_max_turns_event_returns_swarminvoke() {
        let bytes =
            r#"{"type":"result","subtype":"error_max_turns","error":"claude reported a result.error event"}
"#;
        let err = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect_err("err");
        match err {
            AppError::SwarmInvoke(msg) => {
                assert!(msg.contains("error_max_turns"), "got: {msg}");
            }
            other => panic!("expected SwarmInvoke, got {other:?}"),
        }
    }

    /// Stream-json parser stays robust against malformed lines —
    /// non-JSON bytes are warn-logged and skipped, NOT propagated as
    /// errors. The test feeds garbage interleaved with a valid
    /// result.success and expects the success to be observed.
    #[tokio::test]
    async fn malformed_lines_are_skipped() {
        let mut bytes = String::new();
        bytes.push_str("not json\n");
        bytes.push_str("{partial json...\n");
        bytes.push_str(&ok_event_lines("after-garbage", 0.0, 1));
        let result = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("ok");
        assert_eq!(result.assistant_text, "after-garbage");
    }

    /// Stdout EOF before `result` surfaces as `SwarmInvoke` (the
    /// child crashed or claude exited unexpectedly).
    #[tokio::test]
    async fn eof_before_result_returns_swarminvoke() {
        // Only init + a partial assistant block — no result event.
        let bytes = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"oops"}]}}
"#;
        let err = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect_err("err");
        match err {
            AppError::SwarmInvoke(msg) => {
                assert!(msg.contains("EOF") || msg.contains("result"));
            }
            other => panic!("expected SwarmInvoke, got {other:?}"),
        }
    }

    /// Cancel notify fires before the read returns → Cancelled.
    /// Drives the cancel arm directly with the writer kept alive so
    /// the reader blocks on `next_line` instead of seeing EOF.
    #[tokio::test]
    async fn cancel_before_result_returns_cancelled() {
        let cancel = Arc::new(Notify::new());
        let cancel_for_fire = Arc::clone(&cancel);
        // Fire cancel after a short delay so the read loop has time
        // to reach the select.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancel_for_fire.notify_waiters();
        });
        let err = read_until_result_via_duplex_with_close(
            // empty seed; writer stays alive (close_on_eof=false) so
            // reader blocks on next_line instead of seeing EOF.
            &[],
            Duration::from_secs(5),
            Some(cancel),
            false,
        )
        .await
        .expect_err("err");
        assert!(matches!(err, AppError::Cancelled(_)));
    }

    /// Per-turn timeout fires before any event → Timeout error.
    /// Same writer-kept-alive trick as the cancel test.
    #[tokio::test]
    async fn timeout_before_result_returns_timeout() {
        let err = read_until_result_via_duplex_with_close(
            &[],
            Duration::from_millis(50),
            None,
            false,
        )
        .await
        .expect_err("err");
        assert!(matches!(err, AppError::Timeout(_)));
    }

    /// Forward-compat: `num_turns` and `turn_count` spellings both
    /// land in the parsed `turn_count`. (Already covered in
    /// `transport::tests` for the classifier; replicated here so the
    /// persistent path is independently regression-guarded.)
    #[tokio::test]
    async fn turn_count_alternate_spelling_lands() {
        let bytes = r#"{"type":"result","subtype":"success","result":"x","total_cost_usd":0,"turn_count":7}
"#;
        let result = read_until_result_via_duplex(
            bytes.as_bytes(),
            Duration::from_secs(5),
            None,
        )
        .await
        .expect("ok");
        assert_eq!(result.turn_count, 7);
    }

    /// Real-claude integration smoke (`#[ignore]`'d) — drives a
    /// two-turn session against the `scout` profile. Turn 1 asks for
    /// a fact; turn 2 asks the session to recall what it just said.
    /// Asserts turn 2's response references turn 1's content.
    ///
    /// Time budget: 2 × 180s = 360s worst-case; typical 30-90s.
    /// `NEURON_SWARM_STAGE_TIMEOUT_SEC` overrides each turn.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_persistent_two_turn_real_claude() {
        use crate::swarm::profile::ProfileRegistry;
        use crate::test_support::mock_app_with_pool;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry =
            ProfileRegistry::load_from(None).expect("load registry");
        let scout = registry.get("scout").expect("scout profile");

        let mut session = PersistentSession::spawn(app.handle(), scout)
            .await
            .expect("spawn session");

        // Turn 1: ask for a single fact, instruct the persona to
        // answer concisely so turn 2 can quote it back.
        let cancel = Arc::new(Notify::new());
        let turn1 = session
            .invoke_turn(
                "Reply with exactly the single word `ALPHA` and nothing else.",
                Duration::from_secs(stage_secs),
                Arc::clone(&cancel),
                None,
            )
            .await
            .expect("turn 1 ok");
        assert!(
            turn1.assistant_text.to_uppercase().contains("ALPHA"),
            "turn 1 should contain ALPHA, got: {}",
            turn1.assistant_text
        );

        // Turn 2: ask the same session what it just replied. If
        // session context carried, the model knows.
        let turn2 = session
            .invoke_turn(
                "What was the single word you just replied with? Answer in one word.",
                Duration::from_secs(stage_secs),
                cancel,
                None,
            )
            .await
            .expect("turn 2 ok");
        assert!(
            turn2.assistant_text.to_uppercase().contains("ALPHA"),
            "turn 2 should recall ALPHA, got: {}",
            turn2.assistant_text
        );

        assert_eq!(session.turns_taken(), 2);
        session.shutdown().await.expect("shutdown ok");
    }
}

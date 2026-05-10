//! `SubprocessTransport` — one-shot `claude` CLI invocation (WP-W3-11 §4).
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

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use tauri::{AppHandle, Manager, Runtime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

use crate::error::AppError;
use crate::swarm::binding::{
    build_specialist_args, resolve_claude_binary, subscription_env,
};
use crate::swarm::profile::Profile;

/// 64 KiB upper bound on the stderr ring buffer. Generous enough to
/// hold a full `claude` traceback; small enough that the bound is
/// hit only on adversarial output.
///
/// Pub-within-crate so `persistent_session.rs` can dimension its own
/// stderr drain to the same budget without re-litigating the size.
pub(crate) const STDERR_RING_CAPACITY: usize = 64 * 1024;

/// One classified line from the `claude` stream-json output. Mid-loop
/// state (running assistant text, captured `session_id`) lives in the
/// caller's accumulator; this enum is the parser's *event* output.
///
/// Public-within-crate so `tests` and future state-machine consumers
/// can drive the parser independently of the spawn side.
///
/// As of W4-03, a single `assistant` line can produce multiple events
/// (one `AssistantDelta` per text block PLUS one `ToolUse` per
/// tool_use block) — `classify_event` returns `Vec<StreamEvent>`
/// instead of a scalar.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StreamEvent {
    /// `system.init` — carries the subprocess session id.
    SystemInit {
        session_id: String,
    },
    /// `assistant` — one text delta to append to the running buffer.
    AssistantDelta {
        text: String,
    },
    /// W4-03 — `assistant` content carries a `tool_use` block.
    /// Surfaced as a separate event so the live-feed UI can render
    /// "Scout is reading X" while the model continues streaming text.
    ToolUse {
        name: String,
        /// One-line truncation of the tool input (capped ~120 chars).
        /// Full input lives in the eventual `ResultSuccess`.
        input_summary: String,
    },
    /// `result.success` — final answer + accounting; reader stops.
    ResultSuccess {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// `result.error` — the model produced an error result; reader
    /// bails with `AppError::SwarmInvoke`.
    ResultError {
        reason: String,
    },
    /// Recognised but uninteresting (`ping`, `system.compact_boundary`,
    /// etc.) — the caller silently keeps reading. Unused as of W4-03
    /// (uninteresting events now land as an empty `Vec<StreamEvent>`
    /// from `classify_event`); the variant is retained for forward
    /// compatibility — a future event type that we want to log but
    /// not act on can be classified as `Other` rather than threading
    /// a separate enum through.
    #[allow(dead_code)]
    Other,
}

/// Output of one successful `SubprocessTransport::invoke`.
///
/// `total_cost_usd` is what `claude` reports in the `result` event;
/// `turn_count` reflects the number of model turns the child consumed
/// before finishing (≤ `Profile.max_turns`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct InvokeResult {
    pub session_id: String,
    pub assistant_text: String,
    pub total_cost_usd: f64,
    pub turn_count: u32,
}

/// Abstraction over "spawn a one-shot specialist call and return
/// its `result` event". WP-W3-12a's FSM was generic over this
/// trait; W5-06 deleted the FSM, so the only surviving consumer
/// today is `swarm:test_invoke` (one-shot persona-test IPC) and
/// `swarm:orchestrator_decide`. The trait is kept since both
/// callers still want a clean seam between the production
/// subprocess driver and any future mock.
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
        let tmp_path = write_persona_tmp(app, profile).await?;

        // 3. Spawn. `kill_on_drop` is the seatbelt — if anything below
        //    panics or returns early, the child gets SIGKILL/TerminateProcess.
        let env = subscription_env();
        let args = build_specialist_args(profile, &tmp_path);

        let mut child = Command::new(&binary.path)
            .envs(&env)
            // Drop env that `subscription_env` wanted gone — `envs(&env)`
            // doesn't *clear* the inherited slate, so re-strip:
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
                // Best-effort cleanup of the persona file: leaving it
                // around when we never even spawned the binary is
                // pure clutter; the persona is also embedded in the
                // running registry so we lose nothing.
                let _ = std::fs::remove_file(&tmp_path);
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

        // 10. Happy path: clean up the persona tmp file. Errors are
        //     logged at debug level — leaving a stray file is not
        //     load-bearing.
        if let Err(e) = std::fs::remove_file(&tmp_path) {
            tracing::debug!(
                path = %tmp_path.display(),
                error = %e,
                "could not remove persona tmp file (non-fatal)"
            );
        }

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

/// Cap on the per-tool-use `input_summary` string fed into the
/// `StreamEvent::ToolUse` event. Long tool inputs (e.g. a giant
/// `Read` of a multi-MiB file path string) would otherwise spam the
/// live-feed UI; ~120 chars is enough for "path: ..." or
/// "pattern: ..., glob: ..." style summaries.
pub(crate) const TOOL_USE_INPUT_SUMMARY_CAP: usize = 120;

/// Classify one parsed JSON event from the `claude` stream-json
/// output into 0 or more `StreamEvent`s. Pulled out as a synchronous
/// helper so unit tests can drive the line parser without spawning a
/// real subprocess.
///
/// Returns:
/// - `vec![]` for ignored / forward-compat events (returned as
///   `Other` semantics — empty vec is the new "skip this line").
/// - 1 event for `system.init` / `result.*`.
/// - N events for `assistant` lines: one `AssistantDelta` if the
///   content has any text blocks (concatenated) PLUS one `ToolUse`
///   per tool_use block. Order matches the order they appear in the
///   `content` array, with text concatenated up-front, so the
///   caller can apply them in stream order.
pub(crate) fn classify_event(value: &Value) -> Vec<StreamEvent> {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match event_type {
        "system" => {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("");
            if subtype == "init" {
                if let Some(id) =
                    value.get("session_id").and_then(Value::as_str)
                {
                    return vec![StreamEvent::SystemInit {
                        session_id: id.to_string(),
                    }];
                }
            }
            vec![]
        }
        "assistant" => classify_assistant_blocks(value),
        "result" => classify_result_event(value),
        _ => vec![],
    }
}

/// `assistant` line classifier. Walks `message.content[]`, concatenates
/// `text` blocks into one `AssistantDelta`, emits one `ToolUse` per
/// `tool_use` block.
fn classify_assistant_blocks(value: &Value) -> Vec<StreamEvent> {
    let blocks = match value
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    {
        Some(arr) => arr,
        None => return vec![],
    };
    let mut events: Vec<StreamEvent> = Vec::new();
    let mut text_buf = String::new();
    for block in blocks {
        let block_type =
            block.get("type").and_then(Value::as_str).unwrap_or("");
        match block_type {
            "text" => {
                if let Some(t) = block.get("text").and_then(Value::as_str)
                {
                    text_buf.push_str(t);
                }
            }
            "tool_use" => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let input_summary = summarize_tool_input(block.get("input"));
                events.push(StreamEvent::ToolUse {
                    name,
                    input_summary,
                });
            }
            _ => {}
        }
    }
    if !text_buf.is_empty() {
        events.insert(
            0,
            StreamEvent::AssistantDelta { text: text_buf },
        );
    }
    events
}

/// `result` line classifier — unchanged shape vs pre-W4-03; just
/// pulled out for symmetry with `classify_assistant_blocks`.
fn classify_result_event(value: &Value) -> Vec<StreamEvent> {
    let subtype = value
        .get("subtype")
        .and_then(Value::as_str)
        .unwrap_or("");
    match subtype {
        "success" => {
            let assistant_text = value
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let total_cost_usd = value
                .get("total_cost_usd")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            // The CLI is inconsistent here across versions; we
            // accept either spelling so the transport survives a
            // minor bump.
            let turn_count = value
                .get("num_turns")
                .or_else(|| value.get("turn_count"))
                .and_then(Value::as_u64)
                .unwrap_or(0)
                as u32;
            vec![StreamEvent::ResultSuccess {
                assistant_text,
                total_cost_usd,
                turn_count,
            }]
        }
        "error" | "error_max_turns" | "error_during_execution" => {
            let reason = value
                .get("error")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .unwrap_or("claude reported a result.error event")
                .to_string();
            vec![StreamEvent::ResultError {
                reason: format!("{subtype}: {reason}"),
            }]
        }
        _ => vec![],
    }
}

/// Build a short "k1: v1, k2: v2" summary of a tool-input JSON
/// object, capped at `TOOL_USE_INPUT_SUMMARY_CAP` chars (with a
/// trailing "…" when truncated).
fn summarize_tool_input(input: Option<&Value>) -> String {
    let input = match input {
        Some(v) => v,
        None => return String::new(),
    };
    let summary = match input {
        Value::Object(map) => {
            let mut parts: Vec<String> = Vec::with_capacity(map.len());
            for (k, v) in map {
                let v_str = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                parts.push(format!("{k}: {v_str}"));
            }
            parts.join(", ")
        }
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    truncate_with_ellipsis(&summary, TOOL_USE_INPUT_SUMMARY_CAP)
}

/// Truncate a string to at most `cap` chars (counted by `chars()`
/// so we don't slice mid-codepoint), append "…" when truncated.
fn truncate_with_ellipsis(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String =
        s.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
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

/// Tail-only ring buffer. `append` truncates oldest bytes when full.
///
/// Pub-within-crate so `persistent_session.rs` reuses the same shape
/// for its own stderr drain.
pub(crate) struct RingBuffer {
    buf: Vec<u8>,
    capacity: usize,
}

impl RingBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity.min(8 * 1024)),
            capacity,
        }
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            // New burst alone exceeds capacity — keep only its tail.
            let start = bytes.len() - self.capacity;
            self.buf.clear();
            self.buf.extend_from_slice(&bytes[start..]);
            return;
        }
        let combined = self.buf.len() + bytes.len();
        if combined > self.capacity {
            let drop = combined - self.capacity;
            self.buf.drain(..drop);
        }
        self.buf.extend_from_slice(bytes);
    }

    pub(crate) fn tail_string(&self, max_bytes: usize) -> String {
        let start = self.buf.len().saturating_sub(max_bytes);
        String::from_utf8_lossy(&self.buf[start..]).into_owned()
    }
}

pub(crate) fn fmt_stderr_tail(tail: &str) -> String {
    let trimmed = tail.trim();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" — stderr tail: {trimmed}")
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    /// WP §7 — feed the line classifier a stream-json fixture and
    /// confirm it produces the correct sequence of `StreamEvent`s.
    /// W4-03 extended `classify_event` to return `Vec<StreamEvent>`.
    #[test]
    fn stream_json_line_parser() {
        // 1. system.init
        let init: Value = serde_json::from_str(
            r#"{"type":"system","subtype":"init","session_id":"sess-abc"}"#,
        )
        .unwrap();
        assert_eq!(
            classify_event(&init),
            vec![StreamEvent::SystemInit {
                session_id: "sess-abc".into()
            }]
        );

        // 2. assistant text delta (single text block).
        let asst1: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello "}]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_event(&asst1),
            vec![StreamEvent::AssistantDelta {
                text: "Hello ".into()
            }]
        );

        // 3. assistant text + tool_use (W4-03 emits BOTH events;
        //    text first, tool_use after).
        let asst2: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"path":"x.rs"}},{"type":"text","text":"world"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_event(&asst2),
            vec![
                StreamEvent::AssistantDelta {
                    text: "world".into()
                },
                StreamEvent::ToolUse {
                    name: "Read".into(),
                    input_summary: "path: x.rs".into(),
                },
            ]
        );

        // 4. result.success — final answer, cost, turn count.
        let result: Value = serde_json::from_str(
            r#"{"type":"result","subtype":"success","result":"Hello world","total_cost_usd":0.0123,"num_turns":2}"#,
        )
        .unwrap();
        let events = classify_event(&result);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ResultSuccess {
                assistant_text,
                total_cost_usd,
                turn_count,
            } => {
                assert_eq!(assistant_text, "Hello world");
                assert!((total_cost_usd - 0.0123).abs() < 1e-9);
                assert_eq!(*turn_count, 2);
            }
            other => panic!("expected ResultSuccess, got {other:?}"),
        }

        // 5. result.error — surfaced as ResultError with both subtype
        //    and reason in the message.
        let err: Value = serde_json::from_str(
            r#"{"type":"result","subtype":"error","error":"OAuth expired"}"#,
        )
        .unwrap();
        let events = classify_event(&err);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ResultError { reason } => {
                assert!(reason.contains("error"));
                assert!(reason.contains("OAuth expired"));
            }
            other => panic!("expected ResultError, got {other:?}"),
        }

        // 6. Forward-compat — unknown types produce empty Vec
        //    (caller skips them).
        let unknown: Value =
            serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(classify_event(&unknown).is_empty());
    }

    /// `turn_count` should also pick up the `turn_count` spelling
    /// (forward-compat with a future CLI rename).
    #[test]
    fn stream_json_accepts_alternate_turn_count_spelling() {
        let v: Value = serde_json::from_str(
            r#"{"type":"result","subtype":"success","result":"ok","total_cost_usd":0,"turn_count":4}"#,
        )
        .unwrap();
        let events = classify_event(&v);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ResultSuccess { turn_count, .. } => {
                assert_eq!(*turn_count, 4);
            }
            other => panic!("expected ResultSuccess, got {other:?}"),
        }
    }

    /// W4-03 — tool_use block alone (no text) emits a single
    /// `ToolUse` event.
    #[test]
    fn classify_event_parses_tool_use_alone() {
        let v: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Glob","input":{"pattern":"*.rs"}}]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_event(&v),
            vec![StreamEvent::ToolUse {
                name: "Glob".into(),
                input_summary: "pattern: *.rs".into(),
            }]
        );
    }

    /// W4-03 — long tool input is truncated to ~120 chars with a
    /// trailing ellipsis so log spam is bounded.
    #[test]
    fn classify_event_truncates_long_tool_input() {
        let huge = "x".repeat(500);
        let line = format!(
            r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Read","input":{{"path":"{huge}"}}}}]}}}}"#
        );
        let v: Value = serde_json::from_str(&line).unwrap();
        let events = classify_event(&v);
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ToolUse { name, input_summary } => {
                assert_eq!(name, "Read");
                assert_eq!(
                    input_summary.chars().count(),
                    TOOL_USE_INPUT_SUMMARY_CAP
                );
                assert!(input_summary.ends_with('…'));
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    /// W4-03 — tool_use with no `input` field falls back to empty
    /// summary (no panic).
    #[test]
    fn classify_event_tool_use_without_input() {
        let v: Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"BashSummary"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            classify_event(&v),
            vec![StreamEvent::ToolUse {
                name: "BashSummary".into(),
                input_summary: String::new(),
            }]
        );
    }

    /// The ring buffer keeps only the most recent `capacity` bytes.
    #[test]
    fn ring_buffer_truncates_oldest() {
        let mut ring = RingBuffer::new(8);
        ring.append(b"abcdefgh"); // fills exactly
        assert_eq!(ring.tail_string(8), "abcdefgh");
        ring.append(b"ij"); // should drop "ab"
        assert_eq!(ring.tail_string(8), "cdefghij");
        // Burst alone exceeds capacity → keep tail.
        ring.append(b"0123456789012345");
        assert_eq!(ring.tail_string(8), "89012345");
    }

    /// Integration smoke (`#[ignore]`) — spawns the real `claude`
    /// binary against the bundled `scout` profile and expects it to
    /// answer the canonical "Say exactly: 'scout-ok' ..." prompt.
    /// CI lacks both the binary and an OAuth session, so this stays
    /// opt-in via `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_smoke_invoke() {
        use crate::swarm::profile::ProfileRegistry;
        use crate::test_support::mock_app_with_pool;

        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry =
            ProfileRegistry::load_from(None).expect("registry load");
        let scout = registry.get("scout").expect("scout exists");
        let transport = SubprocessTransport::new();
        let result = transport
            .invoke(
                app.handle(),
                scout,
                "Say exactly: 'scout-ok' and nothing else.",
                Duration::from_secs(60),
            )
            .await
            .expect("invoke ok");
        assert!(
            result.assistant_text.contains("scout-ok"),
            "expected `scout-ok` in assistant text, got: {}",
            result.assistant_text
        );
    }
}

// WP-W5-06 — the FSM-only `mock_transport` module (MockTransport +
// MockResponse) was deleted alongside `coordinator::fsm`. Brain
// tests use `ScriptedCoordinatorInvoker` (in `swarm::brain`) and
// the dispatcher tests use `agent_dispatcher::tests`'s mocked
// invoker — neither speaks the `Transport` trait directly.


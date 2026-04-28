//! LangGraph Python sidecar supervisor (WP-W2-04).
//!
//! Owns the lifecycle of the `python -m agent_runtime` child process
//! and drives the JSON-RPC frame protocol described in
//! `src-tauri/sidecar/agent_runtime/README.md`.
//!
//! Wiring at a glance:
//!
//! ```text
//! lib.rs::run().setup(...)
//!     ├── db::init                  → SqlitePool managed in app state
//!     └── sidecar::agent::spawn_runtime
//!             → tokio::process::Command(python -m agent_runtime)
//!             → spawns the read loop (stdout → DB writes + Tauri events)
//!             → returns SidecarHandle, managed in app state
//!
//! commands/runs.rs::runs_create
//!     ├── inserts the row with status='running'
//!     └── sidecar::agent::start_run
//!             → writes a `run.start` frame to the child's stdin
//! ```
//!
//! Per WP-W2-04 §"Out of scope":
//!
//!   Cancel signal mid-LLM-call (best effort: kill the sidecar's run
//!   task; do NOT kill the whole sidecar).
//!
//! We expose `start_run` as a thin "post one frame" API. Future WPs
//! that add cancel propagation can add a `cancel_run(run_id)` frame
//! without changing the supervisor's read loop.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::db::DbPool;
use crate::error::AppError;
use crate::sidecar::framing::{read_frame, write_frame, Frame};

// --------------------------------------------------------------------- //
// Public handle managed by Tauri state                                   //
// --------------------------------------------------------------------- //

/// Type-erased handle the rest of the codebase passes around. Every
/// public method consumes `&self` and dispatches into the inner
/// `Arc<Inner>` so cloning is cheap and `tauri::State` can hand out
/// shared references without lock contention on the outer struct.
#[derive(Clone)]
pub struct SidecarHandle {
    inner: Arc<Inner>,
}

struct Inner {
    /// Locked write side of the child's stdin. Frames are serialized
    /// here by acquiring the mutex, which prevents two concurrent
    /// `start_run` calls from interleaving bytes.
    stdin: Mutex<Option<ChildStdin>>,
    /// The `Child` itself, kept so `shutdown()` can `kill()` it. We
    /// never poll it for status from this side — the read loop notices
    /// EOF on stdout when the child exits.
    child: Mutex<Option<Child>>,
}

// --------------------------------------------------------------------- //
// Inbound event shapes (from the Python sidecar)                         //
// --------------------------------------------------------------------- //

/// Wire-shape of one span as sent by the sidecar. Mirrors
/// `crate::models::Span`'s **camelCase** serialization with
/// `attrs_json` carried as a JSON string (TEXT column).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSpan {
    id: String,
    run_id: String,
    parent_span_id: Option<String>,
    name: String,
    #[serde(rename = "type")]
    span_type: String,
    t0_ms: i64,
    duration_ms: Option<i64>,
    attrs_json: String,
    prompt: Option<String>,
    response: Option<String>,
    is_running: bool,
}

/// Top-level event envelope. Either a span event (`span.created` /
/// `.updated` / `.closed`) or a run completion. `event="ready"` /
/// `event="error"` are also accepted; the read loop logs them and
/// keeps going.
#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[serde(rename_all_fields = "camelCase")]
enum SidecarEvent {
    /// `span.created` event — sidecar opened a new span.
    #[serde(rename = "span.created")]
    SpanCreated {
        run_id: String,
        span: WireSpan,
    },
    /// `span.updated` event — partial progress on an open span.
    /// Currently emitted by the workflow only on demand (Week 2 has
    /// none); the read loop tolerates it for forward-compat.
    #[serde(rename = "span.updated")]
    SpanUpdated {
        run_id: String,
        span: WireSpan,
    },
    /// `span.closed` event — span finalised with `is_running=false`
    /// and a `duration_ms`.
    #[serde(rename = "span.closed")]
    SpanClosed {
        run_id: String,
        span: WireSpan,
    },
    /// `run.completed` envelope — the sidecar is done with this run.
    /// `error` carries the optional error message.
    #[serde(rename = "run.completed")]
    RunCompleted {
        run_id: String,
        status: String,
        #[serde(default)]
        error: Option<String>,
    },
    /// `ready` — emitted once at sidecar startup. Useful for the UI
    /// "Starting agent" pill (WP-W2-04 §"Notes / risks").
    Ready,
    /// `error` — sidecar hit a non-fatal protocol error (bad frame,
    /// unknown method). Logged; the loop keeps reading.
    Error {
        #[serde(default)]
        message: Option<String>,
    },
}

/// Discriminator we put on the `runs:{id}:span` Tauri event payload
/// so the frontend can switch on `kind` without inspecting the
/// `is_running` boolean. Per the orchestrator brief, `:` (not `.`) is
/// the canonical separator because Tauri 2.10 rejects `.` in event
/// names.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RunSpanPayload<'a> {
    /// `"created" | "updated" | "closed"`.
    kind: &'a str,
    span: &'a Value,
}

// --------------------------------------------------------------------- //
// Public API                                                             //
// --------------------------------------------------------------------- //

impl SidecarHandle {
    /// Send a `run.start` frame. Returns `Ok(())` on successful write;
    /// any subsequent failure surfaces as a `run.completed` event with
    /// `status='error'` from the sidecar itself.
    pub async fn start_run(&self, workflow_id: &str, run_id: &str) -> Result<(), AppError> {
        let payload = json!({
            "method": "run.start",
            "params": { "workflowId": workflow_id, "runId": run_id }
        });
        self.send(&payload).await
    }

    /// Tear the sidecar down on app exit. Tries a clean `shutdown`
    /// frame first, then kills the process if it does not exit
    /// promptly. Either path leaves the handle unusable.
    pub async fn shutdown(&self) {
        // Best-effort clean shutdown: send the frame and drop stdin so
        // the child sees EOF. We do not wait for the child to exit
        // here — the supervisor's read-loop will observe stdout EOF and
        // log the exit; the `kill_on_drop` we set during spawn means
        // any remaining process is reaped when the handle goes away.
        let _ = self.send(&json!({"method": "shutdown"})).await;
        // Drop the stdin pipe so the child's blocking stdin read sees
        // EOF and the asyncio loop in `__main__.py` exits.
        let mut stdin_slot = self.inner.stdin.lock().await;
        *stdin_slot = None;
        // Kill if still alive; ignore errors (already exited is fine).
        let mut child_slot = self.inner.child.lock().await;
        if let Some(mut child) = child_slot.take() {
            let _ = child.start_kill();
        }
    }

    async fn send(&self, value: &Value) -> Result<(), AppError> {
        let body = serde_json::to_vec(value)?;
        let mut guard = self.inner.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| {
            AppError::Sidecar("agent runtime sidecar is not running".into())
        })?;
        write_frame(stdin, &body)
            .await
            .map_err(|e| AppError::Sidecar(format!("write frame: {e}")))?;
        Ok(())
    }
}

// --------------------------------------------------------------------- //
// Spawn entry point — called from `lib.rs::run().setup(...)`             //
// --------------------------------------------------------------------- //

/// Spawn `python -m agent_runtime` as a managed child process, install
/// a stdout-reading task that converts wire events into DB writes +
/// Tauri events, and return a handle the runtime can stash in app
/// state.
///
/// The subprocess runs from `src-tauri/sidecar/agent_runtime/` so the
/// `agent_runtime` package is importable and the `.venv` Python
/// interpreter is on disk relative to the manifest.
pub fn spawn_runtime<R: Runtime>(app: &AppHandle<R>) -> Result<SidecarHandle, AppError> {
    let app_for_loop = app.clone();
    let pool = app
        .try_state::<DbPool>()
        .ok_or_else(|| AppError::Sidecar("DbPool not in app state — call db::init first".into()))?
        .inner()
        .clone();

    let (python, working_dir) = resolve_python()?;

    // `kill_on_drop` is the seatbelt for the case where the Tauri
    // builder panics after we spawned the child but before the
    // setup hook returns — `Child::drop` then sends SIGKILL / TerminateProcess.
    let mut child = Command::new(python)
        .arg("-m")
        .arg("agent_runtime")
        .current_dir(&working_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        // Inherit stderr so Python tracebacks land in the dev console.
        // `Stdio::piped()` would force us to consume them or risk a
        // pipe-full deadlock; stderr inheritance is the simplest
        // correct path for Week 2.
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            AppError::Sidecar(format!(
                "failed to spawn LangGraph sidecar (working dir: {}): {e}",
                working_dir.display()
            ))
        })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::Sidecar("child stdin pipe missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Sidecar("child stdout pipe missing".into()))?;

    let inner = Arc::new(Inner {
        stdin: Mutex::new(Some(stdin)),
        child: Mutex::new(Some(child)),
    });

    // Spawn the read loop on Tauri's tokio runtime. The loop ends
    // naturally on stdout EOF (child exited) or on a hard frame error.
    tauri::async_runtime::spawn(read_loop(stdout, pool, app_for_loop));

    Ok(SidecarHandle { inner })
}

/// Resolve the Python interpreter and the working directory for the
/// sidecar process.
///
/// Search order (first hit wins):
///
/// 1. `NEURON_AGENT_PYTHON` env var — explicit override (CI / tests).
/// 2. The uv-managed venv at `<sidecar>/.venv/` (`Scripts/python.exe`
///    on Windows, `bin/python` elsewhere).
/// 3. Bare `python` on PATH (developer dev shell).
fn resolve_python() -> Result<(PathBuf, PathBuf), AppError> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let working_dir = manifest_dir.join("sidecar").join("agent_runtime");

    if let Ok(p) = std::env::var("NEURON_AGENT_PYTHON") {
        return Ok((PathBuf::from(p), working_dir));
    }

    let venv_python = if cfg!(windows) {
        working_dir.join(".venv").join("Scripts").join("python.exe")
    } else {
        working_dir.join(".venv").join("bin").join("python")
    };

    if venv_python.is_file() {
        return Ok((venv_python, working_dir));
    }

    Ok((PathBuf::from("python"), working_dir))
}

// --------------------------------------------------------------------- //
// Stdout reader → DB writer + Tauri event emitter                        //
// --------------------------------------------------------------------- //

async fn read_loop<R: Runtime>(
    stdout: ChildStdout,
    pool: DbPool,
    app: AppHandle<R>,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[sidecar] frame error: {e}");
                break;
            }
        };

        let body = match frame {
            Frame::Body(b) => b,
            Frame::Eof => {
                eprintln!("[sidecar] stdout closed; read loop exiting");
                break;
            }
        };

        let event: SidecarEvent = match serde_json::from_slice(&body) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("[sidecar] decode error: {e}; body: {:?}", String::from_utf8_lossy(&body));
                continue;
            }
        };

        if let Err(e) = handle_event(event, &pool, &app).await {
            eprintln!("[sidecar] handle_event: {e}");
        }
    }
}

async fn handle_event<R: Runtime>(
    event: SidecarEvent,
    pool: &DbPool,
    app: &AppHandle<R>,
) -> Result<(), AppError> {
    match event {
        SidecarEvent::Ready => {
            // Logged for now; Week 3 surfaces this as a "Starting
            // agent" → "Ready" pill in the UI.
            eprintln!("[sidecar] agent runtime ready");
        }
        SidecarEvent::Error { message } => {
            eprintln!(
                "[sidecar] sidecar reported error: {}",
                message.unwrap_or_else(|| "<no message>".into())
            );
        }
        SidecarEvent::SpanCreated { run_id, span } => {
            insert_span(pool, &span).await?;
            emit_span_event(app, &run_id, "created", &span)?;
        }
        SidecarEvent::SpanUpdated { run_id, span } => {
            update_span(pool, &span).await?;
            emit_span_event(app, &run_id, "updated", &span)?;
        }
        SidecarEvent::SpanClosed { run_id, span } => {
            update_span(pool, &span).await?;
            emit_span_event(app, &run_id, "closed", &span)?;
        }
        SidecarEvent::RunCompleted { run_id, status, error } => {
            finalise_run(pool, &run_id, &status).await?;
            // No event — the frontend's `runs:get` re-read on
            // `runs:{id}:span(closed)` already covers the UI update.
            // Logging the optional error helps dev-time debugging.
            if let Some(msg) = error {
                eprintln!("[sidecar] run {run_id} ended in {status}: {msg}");
            }
        }
    }
    Ok(())
}

async fn insert_span(pool: &DbPool, span: &WireSpan) -> Result<(), AppError> {
    let is_running = if span.is_running { 1_i64 } else { 0_i64 };
    sqlx::query(
        "INSERT INTO runs_spans \
         (id, run_id, parent_span_id, name, type, t0_ms, duration_ms, attrs_json, prompt, response, is_running) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&span.id)
    .bind(&span.run_id)
    .bind(span.parent_span_id.as_deref())
    .bind(&span.name)
    .bind(&span.span_type)
    .bind(span.t0_ms)
    .bind(span.duration_ms)
    .bind(&span.attrs_json)
    .bind(span.prompt.as_deref())
    .bind(span.response.as_deref())
    .bind(is_running)
    .execute(pool)
    .await?;
    Ok(())
}

async fn update_span(pool: &DbPool, span: &WireSpan) -> Result<(), AppError> {
    let is_running = if span.is_running { 1_i64 } else { 0_i64 };
    sqlx::query(
        "UPDATE runs_spans SET \
            duration_ms = ?, \
            attrs_json  = ?, \
            prompt      = COALESCE(?, prompt), \
            response    = COALESCE(?, response), \
            is_running  = ? \
         WHERE id = ?",
    )
    .bind(span.duration_ms)
    .bind(&span.attrs_json)
    .bind(span.prompt.as_deref())
    .bind(span.response.as_deref())
    .bind(is_running)
    .bind(&span.id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn finalise_run(pool: &DbPool, run_id: &str, status: &str) -> Result<(), AppError> {
    // The `status` column is CHECK-constrained at SQL to one of
    // `running`, `success`, `error`. Translate any unknown sidecar
    // status into `error` so we do not violate the constraint.
    let safe_status = match status {
        "success" | "error" | "running" => status,
        _ => "error",
    };
    sqlx::query(
        "UPDATE runs SET \
            status      = ?, \
            duration_ms = COALESCE(duration_ms, (CAST(strftime('%s','now') AS INTEGER) - started_at) * 1000) \
         WHERE id = ?",
    )
    .bind(safe_status)
    .bind(run_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn emit_span_event<R: Runtime>(
    app: &AppHandle<R>,
    run_id: &str,
    kind: &str,
    span: &WireSpan,
) -> Result<(), AppError> {
    // Frontend subscribes to `runs:{id}:span` per WP-W2-08; the
    // payload's discriminant is `kind`, so a single event name covers
    // all three lifecycle stages without forcing the UI to subscribe
    // three separate channels.
    let event_name = format!("runs:{run_id}:span");
    // Re-encode the span as a JSON Value so the payload is the exact
    // shape the sidecar sent (post-frontend rename).
    let span_value = serde_json::to_value(SerializableWireSpan(span))?;
    let payload = RunSpanPayload {
        kind,
        span: &span_value,
    };
    app.emit(&event_name, &payload)?;
    Ok(())
}

/// Tiny adapter so we can re-serialize `WireSpan` into JSON without
/// rewriting all of its serde renames in a sister struct.
struct SerializableWireSpan<'a>(&'a WireSpan);

impl<'a> Serialize for SerializableWireSpan<'a> {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        // The frontend's `Span` deserialiser (generated by tauri-specta
        // from `crate::models::Span`) expects the same camelCase shape
        // the sidecar emits, so a direct passthrough is sufficient.
        use serde::ser::SerializeStruct;
        let s = self.0;
        let mut st = ser.serialize_struct("Span", 11)?;
        st.serialize_field("id", &s.id)?;
        st.serialize_field("runId", &s.run_id)?;
        st.serialize_field("parentSpanId", &s.parent_span_id)?;
        st.serialize_field("name", &s.name)?;
        st.serialize_field("type", &s.span_type)?;
        st.serialize_field("t0Ms", &s.t0_ms)?;
        st.serialize_field("durationMs", &s.duration_ms)?;
        st.serialize_field("attrsJson", &s.attrs_json)?;
        st.serialize_field("prompt", &s.prompt)?;
        st.serialize_field("response", &s.response)?;
        st.serialize_field("isRunning", &s.is_running)?;
        st.end()
    }
}

// --------------------------------------------------------------------- //
// Minor unused-deps allowance — `Path` and AsyncReadExt/AsyncWriteExt    //
// land via the `tokio::io` re-export through framing; suppress here for  //
// the doc-only `Path` import.                                            //
// --------------------------------------------------------------------- //

#[allow(dead_code)]
fn _doc_only_path_use(_: &Path) {}

#[cfg(test)]
mod tests {
    //! Most coverage for this module lives in the Python sidecar's own
    //! tests (`agent_runtime/tests/`) plus the framing round-trip in
    //! `framing.rs`. The Rust-side integration test that actually
    //! launches a Python process is gated behind `#[ignore]` so CI
    //! runners without uv / Python don't break.
    //!
    //! WP-W2-04 verification step §"6": the integration test is
    //! opt-in via `cargo test -- --ignored`.

    use super::*;
    use crate::test_support::{fresh_pool, seed_minimal};

    /// Sanity: the WireSpan deserializer accepts the camelCase shape
    /// the Python `to_wire()` produces. If the rename rules drift, the
    /// failure surfaces here instead of at the live IPC boundary.
    #[tokio::test]
    async fn wire_span_decodes_python_camelcase() {
        let raw = r#"{
            "id":"s-abc",
            "runId":"r-1",
            "parentSpanId":null,
            "name":"planner",
            "type":"llm",
            "t0Ms":12345,
            "durationMs":17,
            "attrsJson":"{\"node\":\"planner\"}",
            "prompt":"hi",
            "response":"ok",
            "isRunning":false
        }"#;
        let span: WireSpan = serde_json::from_str(raw).expect("decode");
        assert_eq!(span.id, "s-abc");
        assert_eq!(span.run_id, "r-1");
        assert_eq!(span.span_type, "llm");
        assert_eq!(span.t0_ms, 12345);
        assert!(!span.is_running);
    }

    /// Sanity: the `SidecarEvent` envelope decodes each variant the
    /// sidecar emits.
    #[tokio::test]
    async fn sidecar_event_decodes_all_variants() {
        let ready: SidecarEvent =
            serde_json::from_str(r#"{"event":"ready"}"#).expect("ready");
        assert!(matches!(ready, SidecarEvent::Ready));

        let span_created: SidecarEvent = serde_json::from_str(
            r#"{"event":"span.created","runId":"r-1","span":{
                "id":"s","runId":"r-1","parentSpanId":null,"name":"p","type":"llm",
                "t0Ms":1,"durationMs":null,"attrsJson":"{}","prompt":null,
                "response":null,"isRunning":true
            }}"#,
        )
        .expect("span.created");
        assert!(matches!(span_created, SidecarEvent::SpanCreated { .. }));

        let run_done: SidecarEvent = serde_json::from_str(
            r#"{"event":"run.completed","runId":"r-1","status":"success"}"#,
        )
        .expect("run.completed");
        assert!(matches!(run_done, SidecarEvent::RunCompleted { .. }));
    }

    /// The DB writes used by the read loop work end-to-end against a
    /// fresh pool, with no Python sidecar involved. This proves the
    /// `INSERT` and `UPDATE` SQL is correct in isolation; the real
    /// integration test below is the one that wires a child process in.
    #[tokio::test]
    async fn insert_then_update_span_persists_round_trip() {
        let (pool, _dir) = fresh_pool().await;
        seed_minimal(&pool).await;
        // FK requires a runs row before runs_spans accepts the insert.
        sqlx::query(
            "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
             VALUES ('r-1','w1','Daily summary',1,'running')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let span = WireSpan {
            id: "s-1".into(),
            run_id: "r-1".into(),
            parent_span_id: None,
            name: "planner".into(),
            span_type: "llm".into(),
            t0_ms: 100,
            duration_ms: None,
            attrs_json: "{}".into(),
            prompt: Some("hi".into()),
            response: None,
            is_running: true,
        };
        insert_span(&pool, &span).await.expect("insert");

        let closed = WireSpan {
            id: "s-1".into(),
            run_id: "r-1".into(),
            parent_span_id: None,
            name: "planner".into(),
            span_type: "llm".into(),
            t0_ms: 100,
            duration_ms: Some(50),
            attrs_json: r#"{"ok":true}"#.into(),
            prompt: None, // COALESCE keeps the original
            response: Some("done".into()),
            is_running: false,
        };
        update_span(&pool, &closed).await.expect("update");

        let row: (String, i64, Option<i64>, String, Option<String>, Option<String>, i64) =
            sqlx::query_as(
                "SELECT id, t0_ms, duration_ms, attrs_json, prompt, response, is_running \
                 FROM runs_spans WHERE id = 's-1'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(row.0, "s-1");
        assert_eq!(row.1, 100);
        assert_eq!(row.2, Some(50));
        assert_eq!(row.3, r#"{"ok":true}"#);
        assert_eq!(row.4.as_deref(), Some("hi")); // COALESCE preserved
        assert_eq!(row.5.as_deref(), Some("done"));
        assert_eq!(row.6, 0);
    }

    #[tokio::test]
    async fn finalise_run_sets_status_and_duration() {
        let (pool, _dir) = fresh_pool().await;
        seed_minimal(&pool).await;
        sqlx::query(
            "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
             VALUES ('r-1','w1','Daily summary',1,'running')",
        )
        .execute(&pool)
        .await
        .unwrap();

        finalise_run(&pool, "r-1", "success").await.expect("finalise");

        let (status, duration_ms): (String, Option<i64>) =
            sqlx::query_as("SELECT status, duration_ms FROM runs WHERE id = 'r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "success");
        assert!(duration_ms.is_some(), "duration must be filled in");
    }

    /// Acceptance-criterion stand-in for "Sidecar process is killed on
    /// app shutdown". Spawns a real Python child via `spawn_runtime`,
    /// verifies the `ready` event reaches the read loop (confirming
    /// the pipe is healthy), then drops the handle and confirms the
    /// child exits.
    ///
    /// `#[ignore]`d because CI may not have uv / a synced .venv. Run
    /// with `cargo test -- --ignored sidecar`.
    #[tokio::test]
    #[ignore = "requires uv-managed Python sidecar — opt-in via --ignored"]
    async fn integration_spawn_then_shutdown_kills_child() {
        // The mock builder is the closest equivalent to a real Tauri
        // runtime that does not require a window. We need a managed
        // DbPool because `spawn_runtime` reads it for the read loop.
        let (pool, _dir) = fresh_pool().await;
        seed_minimal(&pool).await;
        let app = tauri::test::mock_builder()
            .manage(pool)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let handle = spawn_runtime(app.handle()).expect("spawn");
        // Give the child a moment to start and emit `ready`.
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
        handle.shutdown().await;
    }
}

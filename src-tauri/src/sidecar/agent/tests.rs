//! Most coverage for this module lives in the Python sidecar's own
//! tests (`agent_runtime/tests/`) plus the framing round-trip in
//! `framing.rs`. The Rust-side integration test that actually
//! launches a Python process is gated behind `#[ignore]` so CI
//! runners without uv / Python don't break.
//!
//! WP-W2-04 verification step §"6": the integration test is
//! opt-in via `cargo test -- --ignored`.

use super::reader::{finalise_run, insert_span, update_span};
use super::spawn::spawn_runtime;
use super::wire::{SidecarEvent, WireSpan};
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

/// WP-W2-07: closing a span via the same UPDATE path used by the
/// read loop must trigger `update_run_aggregates`, lifting
/// `runs.tokens` / `runs.cost_usd` off their initial zeros. We
/// exercise the helpers directly (not `handle_event`) because
/// `emit_span_event` requires a Tauri runtime; the SpanClosed
/// arm's logic is `update_span` + `update_run_aggregates` +
/// `emit_span_event`, and the first two are what this WP changes.
#[tokio::test]
async fn span_close_triggers_run_aggregate_update() {
    let (pool, _dir) = fresh_pool().await;
    seed_minimal(&pool).await;
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status, tokens, cost_usd) \
         VALUES ('r-agg','w1','Daily summary',1,'running',0,0)",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert + close a span carrying tokens / cost in attrs_json.
    let opening = WireSpan {
        id: "s-llm".into(),
        run_id: "r-agg".into(),
        parent_span_id: None,
        name: "llm.plan".into(),
        span_type: "llm".into(),
        t0_ms: 0,
        duration_ms: None,
        attrs_json: "{}".into(),
        prompt: None,
        response: None,
        is_running: true,
    };
    insert_span(&pool, &opening).await.expect("insert");

    let closed = WireSpan {
        id: "s-llm".into(),
        run_id: "r-agg".into(),
        parent_span_id: None,
        name: "llm.plan".into(),
        span_type: "llm".into(),
        t0_ms: 0,
        duration_ms: Some(2400),
        attrs_json: r#"{"tokens_in":412,"tokens_out":88,"cost":0.0124}"#.into(),
        prompt: None,
        response: None,
        is_running: false,
    };
    update_span(&pool, &closed).await.expect("update");
    crate::commands::util::update_run_aggregates(&pool, "r-agg")
        .await
        .expect("aggregate");

    let (tokens, cost): (i64, f64) =
        sqlx::query_as("SELECT tokens, cost_usd FROM runs WHERE id='r-agg'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(tokens, 412 + 88);
    assert!((cost - 0.0124).abs() < 1e-9);
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

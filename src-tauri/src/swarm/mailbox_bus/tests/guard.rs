//! WP-W5-05: the workspace-busy guard that rejects a second brain
//! `JobStarted` for a workspace with an in-flight brain job, plus the
//! shutdown cancel fan-out (`cancel_in_flight_brain_jobs`). Both
//! suites share the local `seed_swarm_job` fixture.

use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEvent};
use crate::test_support::mock_app_with_pool;

/// Test helper: seed one `swarm_jobs` row with the supplied
/// shape. Mirrors `swarm::projector::persist_job_init` minus
/// the duplicate-detect branch — tests want a deterministic
/// fixture, not idempotent insert behavior.
async fn seed_swarm_job(
    pool: &DbPool,
    id: &str,
    workspace_id: &str,
    state: &str,
    source: &str,
) {
    sqlx::query(
        "INSERT INTO swarm_jobs \
         (id, workspace_id, goal, created_at_ms, state, retry_count, last_error, finished_at_ms, last_verdict_json, source) \
         VALUES (?, ?, 'g', 0, ?, 0, NULL, NULL, NULL, ?)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(state)
    .bind(source)
    .execute(pool)
    .await
    .expect("seed swarm_jobs row");
}

// -----------------------------------------------------------------
// WP-W5-05 — workspace-busy guard for JobStarted
// -----------------------------------------------------------------

/// Acceptance: `emit_typed(JobStarted)` against a workspace that
/// already has a brain-driven, non-terminal `swarm_jobs` row
/// surfaces `WorkspaceBusy` with the in-flight job's id —
/// without inserting a mailbox row.
#[tokio::test]
async fn emit_typed_rejects_concurrent_job_started_for_same_workspace() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-existing", "ws-1", "scout", "brain").await;
    let bus = MailboxBus::new(pool.clone());

    let err = bus
        .emit_typed(
            app.handle(),
            "ws-1",
            "agent:user",
            "agent:coordinator",
            "second job",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-second".into(),
                workspace_id: "ws-1".into(),
                goal: "g2".into(),
            },
        )
        .await
        .expect_err("rejected");
    assert_eq!(err.kind(), "workspace_busy");
    match err {
        AppError::WorkspaceBusy {
            workspace_id,
            in_flight_job_id,
        } => {
            assert_eq!(workspace_id, "ws-1");
            assert_eq!(in_flight_job_id, "j-existing");
        }
        other => panic!("expected WorkspaceBusy; got {other:?}"),
    }

    // No mailbox row landed for the rejected JobStarted.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mailbox WHERE kind = 'job_started'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
}

/// Acceptance: once the previous brain job is terminal
/// (`done` / `failed`), the bus accepts a fresh JobStarted for
/// the same workspace.
#[tokio::test]
async fn emit_typed_allows_job_started_after_previous_finished() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    // Two prior jobs in the same workspace; both terminal.
    seed_swarm_job(&pool, "j-done", "ws-1", "done", "brain").await;
    seed_swarm_job(&pool, "j-failed", "ws-1", "failed", "brain").await;
    let bus = MailboxBus::new(pool);

    bus.emit_typed(
        app.handle(),
        "ws-1",
        "agent:user",
        "agent:coordinator",
        "fresh job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-fresh".into(),
            workspace_id: "ws-1".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("JobStarted accepted after previous finished");
}

/// Acceptance: brain jobs in different workspaces run in
/// parallel — the guard scopes per workspace_id.
#[tokio::test]
async fn emit_typed_allows_concurrent_job_started_for_different_workspaces() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-ws-a", "ws-a", "build", "brain").await;
    let bus = MailboxBus::new(pool);

    // Different workspace must be accepted even though ws-a is
    // busy.
    bus.emit_typed(
        app.handle(),
        "ws-b",
        "agent:user",
        "agent:coordinator",
        "ws-b job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-ws-b".into(),
            workspace_id: "ws-b".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("ws-b JobStarted accepted while ws-a is busy");

    // Sanity: ws-a is still busy.
    let err = bus
        .emit_typed(
            app.handle(),
            "ws-a",
            "agent:user",
            "agent:coordinator",
            "ws-a second",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-ws-a-2".into(),
                workspace_id: "ws-a".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect_err("ws-a still busy");
    assert_eq!(err.kind(), "workspace_busy");
}

/// Acceptance: an `fsm`-source row in flight does NOT block a
/// brain JobStarted for the same workspace. The two paths
/// coexist until W5-06 deletes the FSM; the bus only gates on
/// brain jobs (W5-06 collapses both into the brain path).
#[tokio::test]
async fn emit_typed_ignores_fsm_source_jobs_for_busy_check() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_swarm_job(&pool, "j-fsm", "ws-1", "scout", "fsm").await;
    let bus = MailboxBus::new(pool);

    bus.emit_typed(
        app.handle(),
        "ws-1",
        "agent:user",
        "agent:coordinator",
        "brain job sharing workspace with fsm job",
        None,
        MailboxEvent::JobStarted {
            job_id: "j-brain".into(),
            workspace_id: "ws-1".into(),
            goal: "g".into(),
        },
    )
    .await
    .expect("brain JobStarted accepted while fsm job in flight");
}

// -----------------------------------------------------------------
// WP-W5-05 — shutdown cancel fan-out
// -----------------------------------------------------------------

/// Acceptance: `cancel_in_flight_brain_jobs` emits one
/// `MailboxEvent::JobCancel` for every brain-driven, non-terminal
/// `swarm_jobs` row — and skips terminal jobs and FSM-source
/// jobs. Mirrors the body of the `RunEvent::ExitRequested`
/// shutdown hook in `lib.rs::run` so the WP-W5-05 step-3
/// invariant is unit-testable without booting the runtime
/// closure.
#[tokio::test]
async fn shutdown_emits_job_cancel_for_each_in_flight_brain_job() {
    let (app, pool, _dir) = mock_app_with_pool().await;

    // Two in-flight brain jobs across two workspaces…
    seed_swarm_job(&pool, "j-brain-a", "ws-a", "scout", "brain").await;
    seed_swarm_job(&pool, "j-brain-b", "ws-b", "build", "brain").await;
    // …one terminal brain job (must be skipped)…
    seed_swarm_job(&pool, "j-brain-done", "ws-a", "done", "brain").await;
    // …one in-flight FSM-source job (must be skipped — the
    // brain path doesn't drive the FSM's cancel notify).
    seed_swarm_job(&pool, "j-fsm-live", "ws-c", "scout", "fsm").await;

    let bus = MailboxBus::new(pool.clone());

    // Subscribe to each workspace BEFORE the fan-out so the
    // broadcast lands on a live receiver (not strictly required
    // — the SQL log is the source of truth — but it lets us
    // assert per-workspace routing too).
    let mut rx_a = bus.subscribe("ws-a").await;
    let mut rx_b = bus.subscribe("ws-b").await;
    let mut rx_c = bus.subscribe("ws-c").await;

    let emitted =
        bus.cancel_in_flight_brain_jobs(app.handle()).await;
    assert_eq!(emitted, 2, "exactly two in-flight brain jobs cancel");

    // ws-a must receive a JobCancel for j-brain-a.
    let env_a = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx_a.recv(),
    )
    .await
    .expect("ws-a recv within 1s")
    .expect("ws-a envelope");
    match env_a.event {
        MailboxEvent::JobCancel { job_id } => {
            assert_eq!(job_id, "j-brain-a");
        }
        other => panic!("ws-a expected JobCancel; got {other:?}"),
    }

    // ws-b must receive a JobCancel for j-brain-b.
    let env_b = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        rx_b.recv(),
    )
    .await
    .expect("ws-b recv within 1s")
    .expect("ws-b envelope");
    match env_b.event {
        MailboxEvent::JobCancel { job_id } => {
            assert_eq!(job_id, "j-brain-b");
        }
        other => panic!("ws-b expected JobCancel; got {other:?}"),
    }

    // ws-c must NOT receive any cancel (FSM-source job skipped).
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rx_c.recv(),
    )
    .await;
    assert!(
        result.is_err(),
        "ws-c (FSM-source) must not receive a JobCancel; got: {result:?}"
    );

    // Persisted rows: exactly two `kind='job_cancel'` rows in
    // mailbox.
    let cancel_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM mailbox WHERE kind = 'job_cancel'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cancel_count, 2);
}

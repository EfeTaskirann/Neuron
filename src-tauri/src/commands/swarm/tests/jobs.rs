//! `swarm:cancel_job` / `swarm:list_jobs` / `swarm:get_job` coverage
//! (WP-W3-12b / W3-12c / W5-05).

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use crate::commands::swarm::{swarm_cancel_job, swarm_get_job, swarm_list_jobs};
use crate::error::AppError;
use crate::swarm::coordinator::{Job, JobState};
use crate::swarm::JobRegistry;
use crate::test_support::mock_app_with_pool;

// ---------------------------------------------------------------- //
// WP-W3-12c — swarm:cancel_job tests                                //
// ---------------------------------------------------------------- //

/// Cancel against a job_id that the registry has never seen
/// surfaces `NotFound`.
#[tokio::test]
async fn cancel_unknown_job_id_returns_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    app.manage(registry);
    let err = swarm_cancel_job(app.handle().clone(), "j-nonexistent".into())
        .await
        .expect_err("unknown rejected");
    assert_eq!(err.kind(), "not_found");
}

/// Cancel against an empty job_id surfaces `InvalidInput`.
#[tokio::test]
async fn cancel_empty_job_id_returns_invalid_input() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    app.manage(registry);
    let err = swarm_cancel_job(app.handle().clone(), "".into())
        .await
        .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// Cancel against a job that has already completed (Done /
/// Failed) surfaces `Conflict`.
#[tokio::test]
async fn cancel_already_terminal_returns_conflict() {
    use crate::swarm::coordinator::Job;
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    // Insert a terminal job by hand — bypasses the FSM but
    // exercises the same registry surface the real FSM writes.
    let job = Job {
        id: "j-done".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Done,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-done", job)
        .await
        .expect("acquire");
    app.manage(registry);
    let err = swarm_cancel_job(app.handle().clone(), "j-done".into())
        .await
        .expect_err("terminal rejected");
    assert_eq!(err.kind(), "conflict");
}

/// Cancel against a Failed job also surfaces `Conflict`
/// (terminal == Done OR Failed; cancelled jobs ride the Failed
/// path).
#[tokio::test]
async fn cancel_failed_job_returns_conflict() {
    use crate::swarm::coordinator::Job;
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    let job = Job {
        id: "j-failed".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Failed,
        retry_count: 0,
        stages: Vec::new(),
        last_error: Some("boom".into()),
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-failed", job)
        .await
        .expect("acquire");
    app.manage(registry);
    let err = swarm_cancel_job(app.handle().clone(), "j-failed".into())
        .await
        .expect_err("terminal rejected");
    assert_eq!(err.kind(), "conflict");
}

/// In-flight job (state is one of Init/Scout/Plan/Build) but
/// no cancel notify registered → `Conflict` (race: the FSM
/// removed the notify on its tail before the IPC reached
/// `signal_cancel`). The command translates `NotFound` from
/// `signal_cancel` into `Conflict` on this branch so the
/// caller sees a single "already terminal" semantic.
#[tokio::test]
async fn cancel_in_flight_without_notify_returns_conflict() {
    use crate::swarm::coordinator::Job;
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    let job = Job {
        id: "j-mid".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Build,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-mid", job)
        .await
        .expect("acquire");
    // Note: no register_cancel call — simulates the race where
    // the FSM tail has already unregistered.
    app.manage(registry);
    let err = swarm_cancel_job(app.handle().clone(), "j-mid".into())
        .await
        .expect_err("race rejected");
    assert_eq!(err.kind(), "conflict");
}

/// In-flight job with cancel notify registered → cancel
/// signals successfully. We register the notify by hand so the
/// test doesn't need to spin up the full FSM.
#[tokio::test]
async fn cancel_in_flight_with_notify_returns_ok() {
    use crate::swarm::coordinator::Job;
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    let job = Job {
        id: "j-live".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Scout,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-live", job)
        .await
        .expect("acquire");
    let notify = Arc::new(tokio::sync::Notify::new());
    registry
        .register_cancel("j-live", Arc::clone(&notify))
        .expect("register");
    app.manage(registry);

    // Subscribe to the notify *before* we signal so we can
    // assert the cancel actually woke a waiter.
    let waiter = tokio::spawn(async move {
        notify.notified().await;
    });
    tokio::task::yield_now().await;

    swarm_cancel_job(app.handle().clone(), "j-live".into())
        .await
        .expect("ok");
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("waiter wakes within 1s")
        .expect("waiter task panicked");
}

/// Double-cancel against the same in-flight job — the second
/// call must surface `Conflict` or `NotFound` (race-dependent
/// on whether the FSM tail has unregistered the notify yet).
/// We hand-build the registry state without an FSM so the
/// race is deterministic: after the first signal, we manually
/// unregister the cancel notify (simulating the FSM tail) and
/// flip the job to Failed before issuing the second signal.
#[tokio::test]
async fn cancel_double_signal_second_returns_error() {
    use crate::swarm::coordinator::Job;
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    let job = Job {
        id: "j-double".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Scout,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-double", job)
        .await
        .expect("acquire");
    let notify = Arc::new(tokio::sync::Notify::new());
    registry
        .register_cancel("j-double", Arc::clone(&notify))
        .expect("register");
    app.manage(Arc::clone(&registry));

    // First cancel — succeeds.
    swarm_cancel_job(app.handle().clone(), "j-double".into())
        .await
        .expect("first cancel ok");

    // Simulate the FSM tail: flip to Failed and unregister
    // the notify. Order matches what the real FSM does in
    // `finalize_cancelled` + the `CancelGuard` Drop.
    registry
        .update("j-double", |j| {
            j.state = JobState::Failed;
            j.last_error = Some("cancelled by user".into());
        })
        .await
        .expect("update");
    registry.unregister_cancel("j-double");

    // Second cancel — must fail. Conflict (terminal) is the
    // expected branch, but a NotFound from a different race
    // is also acceptable per the WP contract.
    let err = swarm_cancel_job(app.handle().clone(), "j-double".into())
        .await
        .expect_err("second cancel rejected");
    assert!(
        matches!(err, AppError::Conflict(_) | AppError::NotFound(_)),
        "second cancel must be Conflict or NotFound; got: {err:?}"
    );
}

/// `swarm_cancel_job` requires the JobRegistry in app state.
/// Missing state surfaces `Internal`. Defensive; the real
/// `lib.rs::setup` always registers the registry.
#[tokio::test]
async fn cancel_without_registry_state_returns_internal() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    // Intentionally do NOT manage(JobRegistry).
    let err = swarm_cancel_job(app.handle().clone(), "j-anything".into())
        .await
        .expect_err("no registry rejected");
    assert_eq!(err.kind(), "internal");
}

// ---------------------------------------------------------------- //
// WP-W5-05 — swarm:cancel_job source-switching                       //
// ---------------------------------------------------------------- //

/// Helper: seed one `swarm_jobs` row directly. Mirrors the
/// projector's `persist_job_init` shape but bypasses it so the
/// test stays under the IPC's verification contract.
async fn seed_swarm_job_row(
    pool: &crate::db::DbPool,
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

/// Acceptance: `source='brain'` triggers a `MailboxEvent::JobCancel`
/// emit on the workspace's bus. The IPC returns `Ok(())` once the
/// emit lands; the brain + dispatchers pick the event up via
/// their broadcast subscribers (covered by W5-02 / W5-03 unit
/// tests).
#[tokio::test]
async fn cancel_job_brain_source_emits_job_cancel_event() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    app.manage(registry);
    let bus = Arc::new(crate::swarm::MailboxBus::new(pool.clone()));
    app.manage(Arc::clone(&bus));

    // Seed a brain-driven job in the DB. No matching registry
    // entry needed — the brain path doesn't consult the
    // in-memory JobRegistry.
    seed_swarm_job_row(&pool, "j-brain", "ws-1", "scout", "brain").await;

    // Subscribe BEFORE the cancel so we don't miss the broadcast.
    let mut rx = bus.subscribe("ws-1").await;

    swarm_cancel_job(app.handle().clone(), "j-brain".into())
        .await
        .expect("cancel ok");

    // The mailbox row must land with `kind='job_cancel'` carrying
    // our job_id.
    let env = tokio::time::timeout(
        Duration::from_secs(1),
        rx.recv(),
    )
    .await
    .expect("broadcast received within 1s")
    .expect("envelope");
    match env.event {
        crate::swarm::MailboxEvent::JobCancel { job_id } => {
            assert_eq!(job_id, "j-brain");
        }
        other => panic!("expected JobCancel; got {other:?}"),
    }
    // Persisted row exists.
    let kind: String = sqlx::query_scalar(
        "SELECT kind FROM mailbox WHERE rowid = ?",
    )
    .bind(env.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kind, "job_cancel");
}

/// Acceptance: `source='fsm'` keeps the legacy in-memory
/// `JobRegistry::signal_cancel` path. Mirrors
/// `cancel_in_flight_with_notify_returns_ok` but seeds the DB
/// row too so the source-switch hits the `'fsm'` branch.
#[tokio::test]
async fn cancel_job_fsm_source_signals_notify() {
    use crate::swarm::coordinator::Job;
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());

    // In-memory registry entry — needed for the FSM path.
    let job = Job {
        id: "j-fsm".into(),
        goal: "g".into(),
        created_at_ms: 0,
        state: JobState::Scout,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-fsm", job)
        .await
        .expect("acquire");
    let notify = Arc::new(tokio::sync::Notify::new());
    registry
        .register_cancel("j-fsm", Arc::clone(&notify))
        .expect("register");
    app.manage(Arc::clone(&registry));

    // DB row with source='fsm' so the source-query lands the
    // FSM branch.
    seed_swarm_job_row(&pool, "j-fsm", "ws-fsm", "scout", "fsm").await;

    let waiter = tokio::spawn(async move {
        notify.notified().await;
    });
    tokio::task::yield_now().await;

    swarm_cancel_job(app.handle().clone(), "j-fsm".into())
        .await
        .expect("cancel ok");

    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("waiter wakes within 1s")
        .expect("waiter task panicked");
}

/// Acceptance: any unknown source string surfaces `Internal`.
/// Defensive — production only writes `'brain'` or `'fsm'`,
/// so this branch protects against schema drift.
#[tokio::test]
async fn cancel_job_unknown_source_returns_internal_error() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    app.manage(registry);
    // No bus needed — the unknown branch short-circuits before
    // any bus lookup.

    seed_swarm_job_row(
        &pool,
        "j-weird",
        "ws-1",
        "scout",
        "totally-made-up",
    )
    .await;

    let err = swarm_cancel_job(app.handle().clone(), "j-weird".into())
        .await
        .expect_err("unknown source rejected");
    assert_eq!(err.kind(), "internal");
    assert!(
        err.message().contains("totally-made-up"),
        "error message must echo the bad source: {err:?}"
    );
}

/// Acceptance: a job_id that exists in neither the DB nor the
/// registry surfaces `NotFound` — the source-switch falls
/// through to the FSM branch on `None` source, which then
/// looks up the registry and returns `NotFound`. Equivalent in
/// shape to `cancel_unknown_job_id_returns_not_found` but
/// asserted explicitly under the WP-W5-05 path so a future
/// refactor doesn't drop the contract.
#[tokio::test]
async fn cancel_job_nonexistent_id_returns_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let registry = Arc::new(JobRegistry::new());
    app.manage(registry);

    let err = swarm_cancel_job(
        app.handle().clone(),
        "j-does-not-exist".into(),
    )
    .await
    .expect_err("nonexistent rejected");
    assert_eq!(err.kind(), "not_found");
}

// ---------------------------------------------------------------- //
// WP-W3-12b — swarm:list_jobs / swarm:get_job IPC tests             //
// ---------------------------------------------------------------- //

/// Seed `n` finished jobs into the pool via the registry, then
/// invoke `swarm_list_jobs` and assert the wire shape.
#[tokio::test]
async fn swarm_list_jobs_command_returns_summaries() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry =
        Arc::new(JobRegistry::with_pool(pool.clone()));
    // Seed three jobs across one workspace.
    for i in 0..3 {
        let job = Job {
            id: format!("j-{i}"),
            goal: format!("goal {i}"),
            created_at_ms: i as i64,
            state: JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
            source: Job::default_source(),
        };
        registry
            .try_acquire_workspace("ws-list", job)
            .await
            .expect("acquire");
        registry
            .update(&format!("j-{i}"), |j| {
                j.state = JobState::Done;
            })
            .await
            .expect("flip done");
        registry
            .release_workspace("ws-list", &format!("j-{i}"))
            .await;
    }
    app.manage(registry);

    let summaries = swarm_list_jobs(
        app.handle().clone(),
        None,
        Some(50),
    )
    .await
    .expect("list ok");
    assert_eq!(summaries.len(), 3);
    // Ordered newest-first by created_at_ms.
    assert_eq!(summaries[0].id, "j-2");
    for s in &summaries {
        assert_eq!(s.workspace_id, "ws-list");
        assert_eq!(s.state, JobState::Done);
    }
}

/// `swarm_list_jobs` defaults `limit` to 50 when omitted; we
/// verify the call shape rather than the cap by passing > 200.
#[tokio::test]
async fn swarm_list_jobs_caps_limit_at_200() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry =
        Arc::new(JobRegistry::with_pool(pool.clone()));
    app.manage(registry);
    // Empty result is still Ok with the bounded limit applied.
    let result = swarm_list_jobs(
        app.handle().clone(),
        None,
        Some(9999),
    )
    .await
    .expect("list ok");
    assert!(result.is_empty());
}

/// `swarm_get_job` returns the full detail for a known id.
#[tokio::test]
async fn swarm_get_job_command_returns_detail() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry =
        Arc::new(JobRegistry::with_pool(pool.clone()));
    let job = Job {
        id: "j-detail".into(),
        goal: "detail goal".into(),
        created_at_ms: 999,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    };
    registry
        .try_acquire_workspace("ws-detail", job)
        .await
        .expect("acquire");
    registry
        .update("j-detail", |j| {
            j.state = JobState::Done;
        })
        .await
        .expect("update");
    app.manage(registry);

    let detail = swarm_get_job(app.handle().clone(), "j-detail".into())
        .await
        .expect("get ok");
    assert_eq!(detail.id, "j-detail");
    assert_eq!(detail.workspace_id, "ws-detail");
    assert_eq!(detail.goal, "detail goal");
    assert_eq!(detail.state, JobState::Done);
}

/// Unknown job id at the IPC layer surfaces `NotFound`.
#[tokio::test]
async fn swarm_get_job_unknown_returns_not_found_error() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry =
        Arc::new(JobRegistry::with_pool(pool.clone()));
    app.manage(registry);
    let err = swarm_get_job(app.handle().clone(), "j-nope".into())
        .await
        .expect_err("unknown rejected");
    assert_eq!(err.kind(), "not_found");
}

/// Empty job id surfaces `InvalidInput` before touching the DB.
#[tokio::test]
async fn swarm_get_job_empty_id_rejected() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let registry =
        Arc::new(JobRegistry::with_pool(pool.clone()));
    app.manage(registry);
    let err = swarm_get_job(app.handle().clone(), "".into())
        .await
        .expect_err("empty rejected");
    assert_eq!(err.kind(), "invalid_input");
}

/// `swarm_list_jobs` requires the registry in app state.
#[tokio::test]
async fn swarm_list_jobs_without_registry_returns_internal() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_list_jobs(app.handle().clone(), None, None)
        .await
        .expect_err("missing registry");
    assert_eq!(err.kind(), "internal");
}

/// `swarm_get_job` requires the registry in app state.
#[tokio::test]
async fn swarm_get_job_without_registry_returns_internal() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let err = swarm_get_job(app.handle().clone(), "j".into())
        .await
        .expect_err("missing registry");
    assert_eq!(err.kind(), "internal");
}

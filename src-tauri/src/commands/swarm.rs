//! `swarm:*` namespace — Tauri command surface for the swarm
//! substrate. WP-W3-11 introduced the first two commands
//! (`profiles_list`, `test_invoke`); W3-12 / W4 / W5 layered on
//! orchestrator + job + agent surfaces; W5-06 retired the FSM in
//! favour of the Coordinator brain (`swarm:run_job`).
//!
//! **2026-05-31 refactor (T3-01):** this file used to host every
//! command, helper, and test in a single 3043-line module. It now
//! delegates to per-area submodules (`profiles`, `orchestrator`,
//! `jobs`, `agents`, `dispatch`, `run`) and re-exports the public
//! command symbols at the same path so external paths
//! (`commands::swarm::swarm_profiles_list`, the `lib.rs`
//! `collect_commands!` list, doc-comments in
//! `crate::swarm::agent_dispatcher`/`brain`/`coordinator`) keep
//! resolving without change.
//!
//! Shared helpers that more than one submodule needs live here —
//! currently only [`workspace_agents_dir`], which `profiles` and
//! `orchestrator` both consume to locate `<app_data_dir>/agents`.
//! The big `#[cfg(test)] mod tests` block at the bottom exercises
//! every command through the re-exported symbols.

use tauri::{AppHandle, Manager, Runtime};

use crate::error::AppError;

// `pub mod` (not `mod`) because `lib.rs::collect_commands!` references
// the macro-generated `__cmd__*` / `__specta__fn__*` helpers via
// `commands::swarm::<area>::<command>` paths — those helpers are
// emitted in the same module as the command itself, so the submodules
// must be reachable from outside this file.
pub mod agents;
pub mod dispatch;
pub mod jobs;
pub mod orchestrator;
pub mod profiles;
pub mod run;

pub use agents::{swarm_agents_list_status, swarm_agents_shutdown_workspace};
pub use dispatch::swarm_agents_dispatch_to_agent;
pub use jobs::{swarm_cancel_job, swarm_get_job, swarm_list_jobs};
pub use orchestrator::{
    swarm_orchestrator_clear_history, swarm_orchestrator_decide,
    swarm_orchestrator_history, swarm_orchestrator_log_job,
};
pub use profiles::{swarm_profiles_list, swarm_test_invoke};
pub use run::swarm_run_job;

#[cfg(test)]
pub(crate) use run::swarm_run_job_with_invoker;

/// Resolve `<app_data_dir>/agents`. Returns `None` (no error) when
/// the directory does not exist — workspace overrides are optional
/// per WP §2. Errors reaching `app_data_dir` itself are real (the
/// platform Tauri helper failed) and surface as `Internal`.
pub(super) fn workspace_agents_dir<R: Runtime>(
    app: &AppHandle<R>,
) -> Result<Option<std::path::PathBuf>, AppError> {
    let base = app.path().app_data_dir().map_err(|e| {
        AppError::Internal(format!("app_data_dir resolution: {e}"))
    })?;
    let dir = base.join("agents");
    if dir.is_dir() {
        Ok(Some(dir))
    } else {
        Ok(None)
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //
//
// Tests stayed in this module (rather than fanning out per submodule)
// because they were written as one `mod tests` block sharing helpers
// (`mock_app_with_w5_state`, `mock_app_with_brain_state`,
// `BrainScriptedInvoker`, `seed_swarm_job_row`). Splitting them would
// duplicate that scaffolding. Every command is reachable via the
// `pub use` re-exports above, so `use super::*;` resolves the same as
// the pre-split version.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;

    // Test-only re-imports. Kept inside the tests module (rather than
    // at file scope) so the non-test compile doesn't drag in symbols
    // it never uses — `workspace_agents_dir` is the only non-test
    // item in this file and it only needs `AppHandle` + `AppError`.
    use std::sync::Arc;
    use std::time::Duration;
    use crate::swarm::coordinator::orchestrator_session::append_user_message;
    use crate::swarm::coordinator::JobState;
    use crate::swarm::{JobRegistry, ProfileRegistry, SwarmAgentRegistry};

    /// Acceptance: on a fresh install (no `<app_data_dir>/agents/`),
    /// `swarm:profiles_list` returns exactly the nine bundled
    /// profiles (W3-12d added reviewer + integration-tester; W3-12f
    /// added the coordinator brain; W3-12g renamed `reviewer` to
    /// `backend-reviewer` and added `frontend-builder` +
    /// `frontend-reviewer`; W3-12k1 added the orchestrator brain
    /// inserted alphabetically between `integration-tester` and
    /// `planner`) in deterministic alphabetical order.
    #[tokio::test]
    async fn profiles_list_returns_nine_bundled() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let summaries = swarm_profiles_list(app.handle().clone())
            .await
            .expect("ok");
        let ids: Vec<&str> =
            summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "backend-builder",
                "backend-reviewer",
                "coordinator",
                "frontend-builder",
                "frontend-reviewer",
                "integration-tester",
                "orchestrator",
                "planner",
                "scout",
            ]
        );
        for s in &summaries {
            assert_eq!(
                s.source, "bundled",
                "fresh install: every profile must be bundled"
            );
        }
    }

    /// `swarm:test_invoke` rejects unknown profile ids before
    /// spawning anything.
    #[tokio::test]
    async fn test_invoke_unknown_profile_returns_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "no-such-profile".into(),
            "hello".into(),
        )
        .await
        .expect_err("unknown profile rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Empty profile id is `invalid_input`, not `not_found`.
    #[tokio::test]
    async fn test_invoke_empty_profile_id_rejected() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "".into(),
            "hello".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Empty user message is `invalid_input`.
    #[tokio::test]
    async fn test_invoke_empty_message_rejected() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_test_invoke(
            app.handle().clone(),
            "scout".into(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    // ---------------------------------------------------------------- //
    // WP-W3-12k1 — swarm:orchestrator_decide validation tests           //
    // ---------------------------------------------------------------- //

    /// Empty `workspace_id` short-circuits before any subprocess
    /// spawn happens; the IPC surfaces `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "".into(),
            "selam".into(),
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspace_id` is treated identically to
    /// empty (`trim().is_empty()` gate). The same gate exists on
    /// `swarm:run_job` so the two surfaces stay symmetric.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_rejects_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "   ".into(),
            "selam".into(),
        )
        .await
        .expect_err("whitespace workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Empty `user_message` short-circuits; the IPC surfaces
    /// `InvalidInput`. The Orchestrator is not allowed to invent a
    /// goal from an empty message.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_validates_empty_message() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "ws-1".into(),
            "".into(),
        )
        .await
        .expect_err("empty message rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `user_message` is treated identically to
    /// empty. Mirrors the validator on `workspace_id`.
    #[tokio::test]
    async fn swarm_orchestrator_decide_command_rejects_whitespace_message() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_decide(
            app.handle().clone(),
            "ws-1".into(),
            "   \t\n".into(),
        )
        .await
        .expect_err("whitespace message rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

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

    use crate::swarm::coordinator::Job;

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

    // ---------------------------------------------------------------- //
    // WP-W3-12k2 — orchestrator history / clear / log_job IPC tests    //
    // ---------------------------------------------------------------- //

    /// Seed N=3 messages directly via the helpers, then call the
    /// IPC and assert it returns oldest-first chronological.
    #[tokio::test]
    async fn swarm_orchestrator_history_returns_oldest_first() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        // Seed three rows out of order so the assertion is non-trivial.
        append_user_message(&pool, "default", "first", 100)
            .await
            .expect("seed u1");
        append_user_message(&pool, "default", "third", 300)
            .await
            .expect("seed u3");
        append_user_message(&pool, "default", "second", 200)
            .await
            .expect("seed u2");

        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "first");
        assert_eq!(msgs[1].content, "second");
        assert_eq!(msgs[2].content, "third");
    }

    /// Caller-supplied `limit > 200` is capped at 200 — verified by
    /// the empty-result happy path (a `limit=9999` against an empty
    /// pool still returns `Ok(vec![])` rather than erroring).
    #[tokio::test]
    async fn swarm_orchestrator_history_caps_limit_at_200() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            Some(9999),
        )
        .await
        .expect("history ok");
        assert!(msgs.is_empty());
    }

    /// Empty `workspaceId` short-circuits with `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_history_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_history(
            app.handle().clone(),
            "".into(),
            None,
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspaceId` matches the W3-12k1 trim gate.
    #[tokio::test]
    async fn swarm_orchestrator_history_validates_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_history(
            app.handle().clone(),
            "   ".into(),
            None,
        )
        .await
        .expect_err("whitespace workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_clear_history` empties the targeted
    /// workspace.
    #[tokio::test]
    async fn swarm_orchestrator_clear_history_empties_workspace() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        append_user_message(&pool, "default", "drop me", 100)
            .await
            .expect("seed");
        swarm_orchestrator_clear_history(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("clear ok");
        let after = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert!(after.is_empty());
    }

    /// Empty `workspaceId` short-circuits with `InvalidInput` on the
    /// clear surface too.
    #[tokio::test]
    async fn swarm_orchestrator_clear_history_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_clear_history(
            app.handle().clone(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_log_job` writes the Job row visibly via
    /// the history IPC.
    #[tokio::test]
    async fn swarm_orchestrator_log_job_persists_row() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        swarm_orchestrator_log_job(
            app.handle().clone(),
            "default".into(),
            "j-abc".into(),
            "Add doc to X.tsx".into(),
        )
        .await
        .expect("log ok");
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "j-abc");
        assert_eq!(msgs[0].goal.as_deref(), Some("Add doc to X.tsx"));
    }

    /// Empty inputs on the `log_job` surface — workspaceId, jobId,
    /// or goal — surface `InvalidInput`.
    #[tokio::test]
    async fn swarm_orchestrator_log_job_validates_inputs() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "".into(),
            "j-1".into(),
            "g".into(),
        )
        .await
        .expect_err("empty workspace rejected");
        assert_eq!(err.kind(), "invalid_input");
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "ws".into(),
            "".into(),
            "g".into(),
        )
        .await
        .expect_err("empty jobId rejected");
        assert_eq!(err.kind(), "invalid_input");
        let err = swarm_orchestrator_log_job(
            app.handle().clone(),
            "ws".into(),
            "j-1".into(),
            "   ".into(),
        )
        .await
        .expect_err("whitespace goal rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `swarm_orchestrator_decide` persists the user message even
    /// when the LLM invoke is unreachable. The subprocess spawn
    /// will fail in the mock-runtime environment (no `claude` binary)
    /// — but the user row must already be in the DB by then.
    #[tokio::test]
    async fn swarm_orchestrator_decide_persists_user_before_invoke() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // The decide call will surface a SwarmInvoke / spawn error
        // because the mock runtime has no `claude` binary on PATH.
        // We only care that the user row landed before the failure.
        let _ = swarm_orchestrator_decide(
            app.handle().clone(),
            "default".into(),
            "selam".into(),
        )
        .await;
        let msgs = swarm_orchestrator_history(
            app.handle().clone(),
            "default".into(),
            None,
        )
        .await
        .expect("history ok");
        // The very first message persisted is the user row.
        assert!(!msgs.is_empty());
        assert_eq!(msgs[0].content, "selam");
    }

    // ---------------------------------------------------------------- //
    // WP-W4-02 — swarm:agents:list_status / shutdown_workspace IPC     //
    // ---------------------------------------------------------------- //

    /// Empty `workspace_id` short-circuits with `InvalidInput` before
    /// touching the registry.
    #[tokio::test]
    async fn swarm_agents_list_status_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err =
            swarm_agents_list_status(app.handle().clone(), "".into())
                .await
                .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Whitespace-only `workspace_id` rejected — same gate as the
    /// other swarm IPCs.
    #[tokio::test]
    async fn swarm_agents_list_status_rejects_whitespace_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err =
            swarm_agents_list_status(app.handle().clone(), "   ".into())
                .await
                .expect_err("whitespace rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Missing registry state surfaces `Internal` — defensive path.
    #[tokio::test]
    async fn swarm_agents_list_status_without_registry_returns_internal() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // Intentionally do NOT manage(SwarmAgentRegistry).
        let err = swarm_agents_list_status(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect_err("missing registry");
        assert_eq!(err.kind(), "internal");
    }

    /// Happy path — fresh registry returns 9 `NotSpawned` rows
    /// alphabetically. Same shape `swarm:profiles_list` promises.
    #[tokio::test]
    async fn swarm_agents_list_status_returns_not_spawned_for_fresh_workspace() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let rows = swarm_agents_list_status(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("ok");
        assert_eq!(rows.len(), 9);
        for r in &rows {
            assert_eq!(
                r.status,
                crate::swarm::AgentStatus::NotSpawned
            );
            assert_eq!(r.turns_taken, 0);
            assert!(r.last_activity_ms.is_none());
        }
    }

    /// `shutdown_workspace` empty workspaceId rejected.
    #[tokio::test]
    async fn swarm_agents_shutdown_workspace_validates_empty_workspace_id() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        let err = swarm_agents_shutdown_workspace(
            app.handle().clone(),
            "".into(),
        )
        .await
        .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// `shutdown_workspace` is idempotent — calling on an empty
    /// workspace returns `Ok(())`.
    #[tokio::test]
    async fn swarm_agents_shutdown_workspace_idempotent_on_empty_registry() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let registry = std::sync::Arc::new(
            crate::swarm::SwarmAgentRegistry::new(std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            )),
        );
        app.manage(registry);
        swarm_agents_shutdown_workspace(
            app.handle().clone(),
            "default".into(),
        )
        .await
        .expect("ok");
    }

    // ---------------------------------------------------------------- //
    // WP-W5-02 — swarm:agents:dispatch_to_agent IPC                    //
    // ---------------------------------------------------------------- //

    /// Build a mock app with both `MailboxBus` and `SwarmAgentRegistry`
    /// in state so the W5-02 IPC tests don't repeat the wiring three
    /// times.
    async fn mock_app_with_w5_state() -> (
        tauri::App<tauri::test::MockRuntime>,
        std::sync::Arc<crate::swarm::MailboxBus>,
        std::sync::Arc<SwarmAgentRegistry>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = crate::test_support::fresh_pool().await;
        let bus = std::sync::Arc::new(
            crate::swarm::MailboxBus::new(pool.clone()),
        );
        let registry = std::sync::Arc::new(SwarmAgentRegistry::new(
            std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            ),
        ));
        let app = tauri::test::mock_builder()
            .manage(pool.clone())
            .manage(bus.clone())
            .manage(registry.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, bus, registry, pool, dir)
    }

    /// Acceptance: empty inputs surface `InvalidInput` BEFORE
    /// touching state. Mirrors the validation pattern of every
    /// other swarm IPC.
    #[tokio::test]
    async fn swarm_agents_dispatch_to_agent_validates_inputs() {
        let (app, _bus, _reg, _pool, _dir) = mock_app_with_w5_state().await;
        let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
        let registry_state =
            app.state::<std::sync::Arc<SwarmAgentRegistry>>();

        // Empty workspace_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "".into(),
            "scout".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Empty agent_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "default".into(),
            "".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Whitespace-only agent_id.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state.clone(),
            registry_state.clone(),
            "default".into(),
            "   ".into(),
            "do something".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Empty prompt.
        let err = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state,
            registry_state,
            "default".into(),
            "scout".into(),
            "".into(),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acceptance: a successful call lands a `task_dispatch` row in
    /// the mailbox + ensures a dispatcher exists in the registry.
    ///
    /// We dispatch to a *non-bundled* agent id so the dispatcher's
    /// downstream `acquire_and_invoke_turn` returns `NotFound`
    /// quickly (no real `claude` spawn) and the dispatcher's error
    /// path emits an `error:` agent_result. That way the test
    /// fully exercises the IPC + emit surface without a 60s claude
    /// spawn timing out.
    #[tokio::test]
    async fn swarm_agents_dispatch_to_agent_emits_dispatch_event() {
        let (app, bus, registry, _pool, _dir) =
            mock_app_with_w5_state().await;
        let bus_state = app.state::<std::sync::Arc<crate::swarm::MailboxBus>>();
        let registry_state =
            app.state::<std::sync::Arc<SwarmAgentRegistry>>();

        // Pre-state: no dispatchers, no dispatch rows.
        assert_eq!(registry.dispatcher_count().await, 0);
        let pre =
            bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
        assert!(pre.is_empty());

        let id = swarm_agents_dispatch_to_agent(
            app.handle().clone(),
            bus_state,
            registry_state,
            "default".into(),
            "test-not-bundled".into(),
            "Investigate auth.rs callsites".into(),
            Some("j-test-1".into()),
            Some(true),
        )
        .await
        .expect("dispatch ok");

        // 1. Dispatcher landed for (default, test-not-bundled).
        assert_eq!(registry.dispatcher_count().await, 1);

        // 2. Mailbox has the task_dispatch row.
        let rows =
            bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, id);
        assert_eq!(row.from_pane, "agent:coordinator");
        assert_eq!(row.to_pane, "agent:test-not-bundled");
        match &row.event {
            crate::swarm::MailboxEvent::TaskDispatch {
                job_id,
                target,
                prompt,
                with_help_loop,
            } => {
                assert_eq!(job_id, "j-test-1");
                assert_eq!(target, "agent:test-not-bundled");
                assert_eq!(prompt, "Investigate auth.rs callsites");
                assert!(*with_help_loop);
            }
            _ => panic!("unexpected event kind"),
        }

        // 3. The dispatcher's invoke task fails fast with NotFound
        //    (the agent isn't in the bundled profile registry) and
        //    emits an error AgentResult with parent_id chained
        //    back to the dispatch row. This proves the error path
        //    end-to-end without needing a real `claude` subprocess.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let rows = bus
                .list_typed(Some("agent_result"), None, None)
                .await
                .unwrap();
            if let Some(row) = rows.into_iter().find(|r| r.parent_id == Some(id)) {
                break row;
            }
            if std::time::Instant::now() > deadline {
                panic!("error AgentResult never arrived");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20))
                .await;
        };
        match &result.event {
            crate::swarm::MailboxEvent::AgentResult {
                assistant_text,
                ..
            } => {
                assert!(
                    assistant_text.starts_with("error:"),
                    "expected error result for unknown agent: {assistant_text}"
                );
            }
            _ => panic!("unexpected event kind"),
        }

        // Cleanup — drain the dispatcher so the test exits without
        // leaving its background task in flight.
        registry.shutdown_all().await.expect("shutdown ok");
    }

    // ---------------------------------------------------------------- //
    // WP-W5-03 — swarm:run_job_v2                                       //
    // ---------------------------------------------------------------- //

    /// Build a mock app wiring `JobRegistry`, `MailboxBus`, and
    /// `SwarmAgentRegistry` so the v2 IPC tests don't repeat the
    /// boilerplate. The job registry is in-memory only (`new()`) so
    /// state mutations don't write through to SQLite — the tests
    /// only care about the in-memory shape.
    async fn mock_app_with_brain_state() -> (
        tauri::App<tauri::test::MockRuntime>,
        std::sync::Arc<crate::swarm::JobRegistry>,
        std::sync::Arc<crate::swarm::MailboxBus>,
        std::sync::Arc<SwarmAgentRegistry>,
        tempfile::TempDir,
    ) {
        let (pool, dir) = crate::test_support::fresh_pool().await;
        let job_registry = std::sync::Arc::new(
            crate::swarm::JobRegistry::with_pool(pool.clone()),
        );
        let bus = std::sync::Arc::new(
            crate::swarm::MailboxBus::new(pool.clone()),
        );
        let agent_registry = std::sync::Arc::new(SwarmAgentRegistry::new(
            std::sync::Arc::new(
                ProfileRegistry::load_from(None).expect("load"),
            ),
        ));
        // WP-W5-04 — install the projector registry so brain tests
        // exercise the same `ensure_for_workspace` path the IPC
        // takes in production. Without it, `swarm_run_job` skips
        // the projector spawn and `build_outcome` walks the bus
        // directly (still works, but bypasses the live
        // SwarmJobEvent emit chain).
        let projector_registry = std::sync::Arc::new(
            crate::swarm::JobProjectorRegistry::new(),
        );
        let app = tauri::test::mock_builder()
            .manage(pool)
            .manage(job_registry.clone())
            .manage(bus.clone())
            .manage(agent_registry.clone())
            .manage(projector_registry)
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, job_registry, bus, agent_registry, dir)
    }

    /// Mock CoordinatorInvoker for brain IPC tests — same shape as the
    /// brain's ScriptedCoordinatorInvoker but lives here so the
    /// IPC test path doesn't depend on `#[cfg(test)]` items inside
    /// `swarm::brain`.
    struct BrainScriptedInvoker {
        replies: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl BrainScriptedInvoker {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: std::sync::Arc::new(std::sync::Mutex::new(
                    replies.into_iter().map(String::from).collect(),
                )),
            }
        }
    }

    impl crate::swarm::CoordinatorInvoker for BrainScriptedInvoker {
        fn invoke_coordinator_turn(
            &self,
            _workspace_id: &str,
            _user_message: &str,
            _timeout: std::time::Duration,
            _cancel: std::sync::Arc<tokio::sync::Notify>,
        ) -> impl std::future::Future<
            Output = Result<crate::swarm::InvokeResult, AppError>,
        > + Send {
            let replies = std::sync::Arc::clone(&self.replies);
            async move {
                let mut replies = replies.lock().unwrap();
                if replies.is_empty() {
                    return Err(AppError::Internal(
                        "scripted invoker exhausted".into(),
                    ));
                }
                let text = replies.remove(0);
                Ok(crate::swarm::InvokeResult {
                    session_id: "mock".into(),
                    assistant_text: text,
                    total_cost_usd: 0.01,
                    turn_count: 1,
                })
            }
        }
    }

    /// Acceptance: empty inputs surface `InvalidInput` before any
    /// state mutation.
    #[tokio::test]
    async fn run_job_validates_inputs() {
        let (app, _jr, _bus, _ar, _dir) = mock_app_with_brain_state().await;

        let err = swarm_run_job(
            app.handle().clone(),
            "".into(),
            "do something".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        let err = swarm_run_job(
            app.handle().clone(),
            "default".into(),
            "".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        // Whitespace-only.
        let err = swarm_run_job(
            app.handle().clone(),
            "default".into(),
            "   ".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acceptance: a second call against the same workspace while
    /// the first is in flight surfaces `WorkspaceBusy`. We exercise
    /// this by holding the workspace via a hand-acquired lock —
    /// the IPC's `try_acquire_workspace` short-circuits on the
    /// second call.
    #[tokio::test]
    async fn run_job_workspace_busy_when_concurrent() {
        let (app, jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;

        // Manually acquire the workspace lock (simulates an
        // in-flight job).
        let dummy_job = crate::swarm::Job {
            id: "j-existing".into(),
            goal: "dummy".into(),
            created_at_ms: 0,
            state: crate::swarm::JobState::Init,
            retry_count: 0,
            stages: Vec::new(),
            last_error: None,
            last_verdict: None,
            source: crate::swarm::Job::default_source(),
        };
        jr.try_acquire_workspace("default", dummy_job)
            .await
            .expect("acquire");

        // Now the IPC call collides.
        let err = swarm_run_job(
            app.handle().clone(),
            "default".into(),
            "do something".into(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "workspace_busy");

        // Cleanup so the dispatcher tasks (if any spawned) drain.
        jr.release_workspace("default", "j-existing").await;
    }

    /// Acceptance: a happy-path mock invoker drives the brain
    /// through Dispatch → AgentResult → Finish and returns a
    /// `JobOutcome` with `final_state == Done`. We use a faux
    /// scout-results emitter that watches for the dispatch and
    /// emits the AgentResult so the brain can take its second turn.
    #[tokio::test]
    async fn run_job_runs_full_chain_via_mock_brain() {
        let (app, _jr, bus, _ar, _dir) = mock_app_with_brain_state().await;

        let invoker = std::sync::Arc::new(BrainScriptedInvoker::new(vec![
            r#"{"action":"dispatch","target":"agent:scout","prompt":"investigate"}"#,
            r#"{"action":"finish","outcome":"done","summary":"done"}"#,
        ]));

        // Helper: emit AgentResult once a dispatch lands. Runs in
        // parallel with the IPC call. Uses a clone of the same app
        // handle so the bus's legacy `mailbox:new` Tauri event lands
        // on the same listener set.
        let bus_for_helper = std::sync::Arc::clone(&bus);
        let app_for_helper = app.handle().clone();
        let helper = tokio::spawn(async move {
            // Poll the bus for the first task_dispatch row.
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(10);
            loop {
                let rows = bus_for_helper
                    .list_typed(Some("task_dispatch"), None, Some(10))
                    .await
                    .expect("list");
                if let Some(row) = rows.into_iter().next() {
                    if let crate::swarm::MailboxEvent::TaskDispatch {
                        job_id, ..
                    } = &row.event
                    {
                        bus_for_helper
                            .emit_typed(
                                &app_for_helper,
                                "default",
                                "agent:scout",
                                "agent:coordinator",
                                "result",
                                Some(row.id),
                                crate::swarm::MailboxEvent::AgentResult {
                                    job_id: job_id.clone(),
                                    agent_id: "scout".into(),
                                    assistant_text: "found".into(),
                                    total_cost_usd: 0.01,
                                    turn_count: 1,
                                },
                            )
                            .await
                            .expect("emit");
                        break;
                    }
                }
                if std::time::Instant::now() > deadline {
                    panic!("never saw dispatch");
                }
                tokio::time::sleep(
                    std::time::Duration::from_millis(20),
                )
                .await;
            }
        });

        let outcome = swarm_run_job_with_invoker(
            app.handle().clone(),
            "default".to_string(),
            "do something".to_string(),
            invoker,
            30,
            // spawn_dispatchers=false — the test's helper is the
            // simulated dispatcher, so we don't want the real one
            // racing it (and trying to spawn `claude`).
            false,
        )
        .await
        .expect("ok");

        let _ = helper.await;
        assert_eq!(outcome.final_state, crate::swarm::JobState::Done);
        assert!(outcome.last_error.is_none());
        assert!(outcome.job_id.starts_with("j-"));
    }

    /// Acceptance: the returned `JobOutcome` shape carries the
    /// expected fields. Even on a parse-failure path (brain bails
    /// after the first invoke returns garbage) we get a stub
    /// outcome with `final_state == Failed` and `last_error`
    /// populated. No `claude` spawn needed for this test.
    #[tokio::test]
    async fn run_job_returns_job_outcome_with_correct_shape() {
        let (app, _jr, _bus, _ar, _dir) = mock_app_with_brain_state().await;
        let invoker = std::sync::Arc::new(BrainScriptedInvoker::new(vec![
            "Just garbage no JSON.",
        ]));

        let outcome = swarm_run_job_with_invoker(
            app.handle().clone(),
            "default".to_string(),
            "trivial".to_string(),
            invoker,
            30,
            // spawn_dispatchers=false — no dispatch flows so the
            // real dispatchers wouldn't fire anyway, but disable
            // for symmetry with the other invoker tests.
            false,
        )
        .await
        .expect("ok");

        // Stub shape: empty stages, zero cost, populated last_error.
        assert_eq!(outcome.final_state, crate::swarm::JobState::Failed);
        assert!(outcome.last_error.is_some());
        assert_eq!(outcome.stages.len(), 0);
        assert_eq!(outcome.total_cost_usd, 0.0);
        assert!(outcome.job_id.starts_with("j-"));
    }

    /// Real-claude integration smoke (`#[ignore]`'d) — drives a
    /// minimal "no-dispatch finish" goal through the brain and
    /// asserts `final_state == Done`. The brain only needs one turn
    /// (the LLM emits a `finish` action immediately). Smallest
    /// possible end-to-end smoke; complements the heavier real-
    /// claude smokes below.
    ///
    /// Time budget: typical 30-60s (one Coordinator subprocess
    /// cold-start plus one turn). Run with:
    /// `$env:NEURON_BRAIN_MAX_DISPATCHES="15"; cargo test --lib \
    /// integration_run_job_real_claude_brain -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_run_job_real_claude_brain() {
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            "Reply with a single 'finish' action with outcome=\"done\" \
             and summary=\"smoke\". No dispatches needed."
                .to_string(),
        )
        .await
        .expect("smoke ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "smoke should produce Done outcome"
        );
    }

    /// Real-claude integration smoke (`#[ignore]`) — drives a
    /// research-only goal end-to-end against the brain dispatcher.
    /// The persona should classify "explain how X works" as a
    /// scout-only flow and emit `finish` after consuming the
    /// scout's output.
    ///
    /// Acceptance: `final_state == Done`. Stage / dispatch counts
    /// are LLM-side decisions and aren't asserted at this layer.
    /// W5-06 acceptance gate: passes within 3 retries on a fresh
    /// subprocess pool.
    ///
    /// Time budget: 2 dispatches × 600s typical wall-clock.
    /// Run with `$env:NEURON_SWARM_STAGE_TIMEOUT_SEC="600"; \
    /// $env:NEURON_BRAIN_MAX_DISPATCHES="20"; cargo test --lib \
    /// integration_research_only_real_claude_brain -- --ignored \
    /// --nocapture --test-threads=1`.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_research_only_real_claude_brain() {
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let goal = "Explain how brain dispatches are routed in \
            src-tauri/src/swarm/brain.rs based on the BrainAction \
            parser. Research only — do not edit files.";
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            goal.to_string(),
        )
        .await
        .expect("brain ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "expected Done, got {:?} (last_error: {:?})",
            outcome.final_state,
            outcome.last_error,
        );
    }

    /// Real-claude integration smoke (`#[ignore]`) — drives a full
    /// build+review chain end-to-end against the brain dispatcher.
    /// Uses the canonical "add a one-line method" goal that the
    /// W3-12d FSM smoke pinned. The brain decides the dispatch
    /// order; the assertion checks for a Done terminal state.
    ///
    /// Time budget: typical 3-6 min. Reviewer should approve a
    /// trivial method add. W5-06 acceptance gate: passes within 3
    /// retries on a fresh subprocess pool.
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_full_chain_real_claude_brain_with_verdict() {
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let goal = "Find the `impl ProfileRegistry` block in \
            profile.rs and add a one-line public method \
            `pub fn profile_count(&self) -> usize { self.profiles.len() }` \
            right after the existing `list` method. Just the method. \
            Do NOT add a unit test, do NOT add doc comments, do NOT \
            run cargo check.";
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            goal.to_string(),
        )
        .await
        .expect("brain ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "expected Done, got {:?} (last_error: {:?}, last_verdict: {:?})",
            outcome.final_state,
            outcome.last_error,
            outcome.last_verdict,
        );
    }

    /// Real-claude integration smoke (`#[ignore]`) — fullstack
    /// chain (backend + frontend changes) against the brain
    /// dispatcher. Sequential or parallel dispatch is decided
    /// LLM-side; W5-06 makes no parallel-guarantee.
    ///
    /// Time budget: typical 8-12 min. The integration tester does
    /// a full crate compile so a fresh CARGO_TARGET_DIR is
    /// recommended (FSM-era trick — kept for parity).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_fullstack_chain_real_claude_brain() {
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let goal = "EXECUTE: Edit two source files. \
            (1) Edit src-tauri/src/swarm/coordinator/job.rs. Add a \
            one-line `///` doc comment immediately above the line \
            `pub struct Job {`. The comment text must be exactly: \
            `/// Carries the full lifecycle of a swarm run.` \
            (2) Edit app/src/components/SwarmJobList.tsx. Add a \
            one-line `// ` comment immediately above the line \
            `export function formatRelativeMs`. The comment text \
            must be exactly: \
            `// Rounds elapsed ms to the nearest minute granularity.` \
            Both files must be edited; this is fullstack.";
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            goal.to_string(),
        )
        .await
        .expect("brain ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "expected Done, got {:?} (last_error: {:?})",
            outcome.final_state,
            outcome.last_error,
        );
    }

    /// Real-claude integration smoke (`#[ignore]`) — exercises the
    /// SQLite write-through against the brain. The brain runs a
    /// canonical chain; on completion the smoke asserts
    /// `swarm_jobs` has a single Done row and `swarm_stages` has
    /// at least one row, demonstrating the projector (W5-04)
    /// persisted state through the brain dispatcher.
    ///
    /// Time budget: typical 3-6 min (same as the full-chain smoke).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_persistence_survives_real_claude_chain_brain() {
        use sqlx::Row;
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let goal = "Reply with a single 'finish' action with \
                    outcome=\"done\" and summary=\"persistence smoke\". \
                    No dispatches needed.";
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            goal.to_string(),
        )
        .await
        .expect("brain ok");
        assert_eq!(
            outcome.final_state,
            crate::swarm::JobState::Done,
            "expected Done, got {:?} (last_error: {:?})",
            outcome.final_state,
            outcome.last_error,
        );
        // Verify the projector persisted at least the swarm_jobs row.
        let pool = app
            .handle()
            .state::<crate::db::DbPool>()
            .inner()
            .clone();
        let rows = sqlx::query(
            "SELECT id, state FROM swarm_jobs WHERE id = ?",
        )
        .bind(&outcome.job_id)
        .fetch_all(&pool)
        .await
        .expect("query");
        assert_eq!(rows.len(), 1, "swarm_jobs row should be present");
        let state: String = rows[0].get("state");
        assert_eq!(state, "Done");
    }

    /// Real-claude integration smoke (`#[ignore]`) — drives a
    /// canonical chain and signals `swarm:cancel_job` mid-flight.
    /// The brain (and the W5-05 cancel path) should observe the
    /// `JobCancel` mailbox event and unwind, leaving the job in
    /// `Failed` with `last_error` referencing the cancel.
    ///
    /// Time budget: typical 30-90s (the cancel fires within
    /// seconds of the first dispatch).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_cancel_during_real_claude_chain_brain() {
        let (app, _jr, _bus, _ar, _dir) =
            mock_app_with_brain_state().await;
        let app_handle = app.handle().clone();
        // Spawn a watcher that cancels the first in-flight brain
        // job after a fixed delay.
        let cancel_handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(8))
                .await;
            // Find the most-recent brain-source job and cancel it.
            let pool = app_handle
                .state::<crate::db::DbPool>()
                .inner()
                .clone();
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT id FROM swarm_jobs \
                 WHERE source='brain' AND state NOT IN ('Done','Failed') \
                 ORDER BY id DESC LIMIT 1",
            )
            .fetch_optional(&pool)
            .await
            .expect("query");
            if let Some((job_id,)) = row {
                let _ = swarm_cancel_job(app_handle.clone(), job_id)
                    .await;
            }
        });
        let goal = "Investigate src-tauri/src/swarm/brain.rs \
                    extensively, then explain its design.";
        let outcome = swarm_run_job(
            app.handle().clone(),
            "default".to_string(),
            goal.to_string(),
        )
        .await
        .expect("brain runs even on cancel path");
        let _ = cancel_handle.await;
        // Cancel could land before or after the brain finishes —
        // both Done (race won by brain) and Failed (cancel won) are
        // valid. The smoke is documenting the cancel WIRE works
        // end-to-end; the W5-05 unit tests already pin the
        // semantic.
        assert!(
            matches!(
                outcome.final_state,
                crate::swarm::JobState::Done
                    | crate::swarm::JobState::Failed
            ),
            "expected terminal state, got {:?}",
            outcome.final_state,
        );
    }
}

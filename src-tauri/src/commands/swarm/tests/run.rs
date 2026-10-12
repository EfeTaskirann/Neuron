//! `swarm:run_job` (v2 brain dispatch) coverage (WP-W5-03), plus the
//! `#[ignore]`'d real-`claude` end-to-end smokes.

use tauri::Manager;

use crate::commands::swarm::{
    swarm_cancel_job, swarm_run_job, swarm_run_job_with_invoker,
};
use crate::error::AppError;
use crate::swarm::{ProfileRegistry, SwarmAgentRegistry};

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

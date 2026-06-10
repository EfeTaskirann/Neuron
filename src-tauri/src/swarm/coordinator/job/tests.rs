//! Unit tests for the `coordinator::job` package — `JobState`
//! round-trips, `JobRegistry` acquire/release/cancel surface, and
//! `Job::last_rejecting_gate` / `SwarmJobEvent` wire-shape pins.
//!
//! Tests stayed in one module (rather than fanning out per
//! submodule) because they share the `fixture_job` /
//! `stage_with_verdict` helpers and reach every type through the
//! package re-exports, so `use super::*` resolves the same as the
//! pre-split single-file version.

use super::*;
use std::sync::Arc;
use tokio::sync::Barrier;

use crate::swarm::coordinator::verdict::Verdict;

fn fixture_job(id: &str) -> Job {
    Job {
        id: id.to_string(),
        goal: "test goal".into(),
        created_at_ms: 0,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    }
}

/// Every `JobState` variant serde-roundtrips through the wire
/// shape (specta's camelCase emission) without information loss.
#[test]
fn job_state_transitions_serialize_round_trip() {
    for state in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
        JobState::Done,
        JobState::Failed,
    ] {
        let json =
            serde_json::to_string(&state).expect("serialize");
        let back: JobState =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back, "round-trip failed for {state:?}");
    }
    // Spot-check the on-wire shape so future renames don't
    // silently break the frontend bindings.
    assert_eq!(
        serde_json::to_string(&JobState::Init).unwrap(),
        "\"init\""
    );
    assert_eq!(
        serde_json::to_string(&JobState::Failed).unwrap(),
        "\"failed\""
    );
}

/// `as_db_str` and `from_db_str` round-trip every variant.
/// W3-12b §6 acceptance criterion: "JobState::{as,from}_db_str
/// round-trip on every variant".
#[test]
fn job_state_db_str_round_trips() {
    for state in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
        JobState::Done,
        JobState::Failed,
    ] {
        let s = state.as_db_str();
        let back = JobState::from_db_str(s)
            .unwrap_or_else(|_| panic!("round-trip {state:?} via {s}"));
        assert_eq!(state, back, "db_str round-trip failed for {state:?}");
    }
}

/// Unknown DB-string values surface as `Internal`, not silently
/// mapped to a default state.
#[test]
fn job_state_from_db_str_unknown_errors() {
    let err = JobState::from_db_str("nonsense")
        .expect_err("unknown discriminant rejected");
    assert_eq!(err.kind(), "internal");
}

/// `JobState::is_terminal` matches the documented contract.
#[test]
fn job_state_is_terminal_matches_done_or_failed() {
    assert!(JobState::Done.is_terminal());
    assert!(JobState::Failed.is_terminal());
    for s in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
    ] {
        assert!(!s.is_terminal(), "{s:?} should not be terminal");
    }
}

/// Insert a job, immediately read it back; equality on the
/// non-cloning fields proves the registry stores by value.
#[tokio::test]
async fn job_registry_insert_and_get_roundtrip() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-a", fixture_job("j-1"))
        .await
        .expect("acquire");
    let got = reg.get("j-1").expect("get");
    assert_eq!(got.id, "j-1");
    assert_eq!(got.state, JobState::Init);
    assert_eq!(got.stages.len(), 0);
}

/// `update` mutates the entry in place; `get` reflects it.
#[tokio::test]
async fn job_registry_update_modifies_in_place() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-b", fixture_job("j-2"))
        .await
        .expect("acquire");
    reg.update("j-2", |job| {
        job.state = JobState::Scout;
        job.retry_count = 1;
    })
    .await
    .expect("update");
    let got = reg.get("j-2").expect("get");
    assert_eq!(got.state, JobState::Scout);
    assert_eq!(got.retry_count, 1);

    // Updating a missing id surfaces NotFound.
    let err = reg
        .update("j-missing", |_| {})
        .await
        .expect_err("missing id rejected");
    assert_eq!(err.kind(), "not_found");
}

/// `list` returns every job currently in the registry. The
/// order is unspecified, so we check membership by id.
#[tokio::test]
async fn job_registry_list_returns_all() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-1", fixture_job("j-a"))
        .await
        .expect("ok");
    reg.try_acquire_workspace("ws-2", fixture_job("j-b"))
        .await
        .expect("ok");
    reg.try_acquire_workspace("ws-3", fixture_job("j-c"))
        .await
        .expect("ok");
    let mut ids: Vec<String> =
        reg.list().into_iter().map(|j| j.id).collect();
    ids.sort();
    assert_eq!(ids, vec!["j-a", "j-b", "j-c"]);
}

/// Two concurrent acquires for the SAME `workspace_id` — exactly
/// one returns Ok, the other returns `WorkspaceBusy`. We use a
/// barrier to force both tasks to call `try_acquire_workspace`
/// at the same instant; whichever the OS scheduler runs first
/// wins, but the *count* of winners is always exactly one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn try_acquire_workspace_first_caller_wins() {
    let reg = Arc::new(JobRegistry::new());
    let barrier = Arc::new(Barrier::new(2));

    let r1 = Arc::clone(&reg);
    let b1 = Arc::clone(&barrier);
    let t1 = tokio::spawn(async move {
        b1.wait().await;
        r1.try_acquire_workspace(
            "shared",
            fixture_job("j-thread-1"),
        )
        .await
    });
    let r2 = Arc::clone(&reg);
    let b2 = Arc::clone(&barrier);
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        r2.try_acquire_workspace(
            "shared",
            fixture_job("j-thread-2"),
        )
        .await
    });
    let (r1_out, r2_out) =
        tokio::join!(t1, t2);
    let r1_out = r1_out.expect("task 1 panic");
    let r2_out = r2_out.expect("task 2 panic");

    let oks = [&r1_out, &r2_out]
        .into_iter()
        .filter(|r| r.is_ok())
        .count();
    let errs = [&r1_out, &r2_out]
        .into_iter()
        .filter_map(|r| r.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(oks, 1, "exactly one acquire must win");
    assert_eq!(errs.len(), 1, "exactly one acquire must lose");
    assert_eq!(errs[0].kind(), "workspace_busy");
}

/// Two concurrent acquires for DIFFERENT `workspace_id`s both
/// succeed — no global FSM lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn try_acquire_workspace_different_workspaces_dont_collide() {
    let reg = Arc::new(JobRegistry::new());
    let barrier = Arc::new(Barrier::new(2));

    let r1 = Arc::clone(&reg);
    let b1 = Arc::clone(&barrier);
    let t1 = tokio::spawn(async move {
        b1.wait().await;
        r1.try_acquire_workspace("ws-x", fixture_job("j-x")).await
    });
    let r2 = Arc::clone(&reg);
    let b2 = Arc::clone(&barrier);
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        r2.try_acquire_workspace("ws-y", fixture_job("j-y")).await
    });
    let (r1_out, r2_out) = tokio::join!(t1, t2);
    r1_out.expect("task 1 panic").expect("ws-x ok");
    r2_out.expect("task 2 panic").expect("ws-y ok");
}

/// Acquire, release, re-acquire same workspace → second acquire
/// succeeds.
#[tokio::test]
async fn release_workspace_unlocks_for_subsequent_acquire() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-r", fixture_job("j-first"))
        .await
        .expect("acquire 1");
    reg.release_workspace("ws-r", "j-first").await;
    reg.try_acquire_workspace("ws-r", fixture_job("j-second"))
        .await
        .expect("acquire 2");
}

/// Releasing twice (or against a stale job_id) is a no-op —
/// matches the defensive Drop-guard contract.
#[tokio::test]
async fn release_workspace_is_idempotent() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-d", fixture_job("j-d"))
        .await
        .expect("acquire");
    reg.release_workspace("ws-d", "j-d").await;
    // Second release: no panic, no error surface (release is fn-> ()).
    reg.release_workspace("ws-d", "j-d").await;
    // Stale id (different job): also a no-op — the workspace is
    // free and stays free.
    reg.release_workspace("ws-d", "j-stale").await;
    reg.try_acquire_workspace("ws-d", fixture_job("j-d2"))
        .await
        .expect("acquire after idempotent releases");
}

/// Empty `workspace_id` (or whitespace-only) → `InvalidInput`,
/// not `WorkspaceBusy`. The pre-flight check fires before the
/// lock map is touched.
#[tokio::test]
async fn try_acquire_workspace_empty_id_rejected() {
    let reg = JobRegistry::new();
    for bad in ["", "   ", "\t\n"] {
        let err = reg
            .try_acquire_workspace(bad, fixture_job("j-bad"))
            .await
            .expect_err(&format!("`{bad:?}` should be rejected"));
        assert_eq!(err.kind(), "invalid_input");
    }
}

// ---------------------------------------------------------------
// WP-W3-12c — cancel-notify surface tests
// ---------------------------------------------------------------

/// Registering a cancel notify for a job_id twice surfaces
/// `Conflict` — protects against the (theoretical) double-
/// register that would silently shadow the original Notify.
#[test]
fn register_cancel_duplicate_returns_conflict() {
    let reg = JobRegistry::new();
    let n1 = Arc::new(tokio::sync::Notify::new());
    reg.register_cancel("j-c1", Arc::clone(&n1))
        .expect("first register ok");
    let n2 = Arc::new(tokio::sync::Notify::new());
    let err = reg
        .register_cancel("j-c1", n2)
        .expect_err("second register rejected");
    assert_eq!(err.kind(), "conflict");
}

/// `unregister_cancel` is idempotent — calling against a
/// missing id is a no-op (mirrors `release_workspace`'s
/// contract so the FSM tail + Drop guard can both fire).
#[test]
fn unregister_cancel_is_idempotent() {
    let reg = JobRegistry::new();
    let n = Arc::new(tokio::sync::Notify::new());
    reg.register_cancel("j-u1", Arc::clone(&n))
        .expect("register ok");
    reg.unregister_cancel("j-u1");
    // Second unregister: no panic, no error surface.
    reg.unregister_cancel("j-u1");
    // Stale id (never registered): also a no-op.
    reg.unregister_cancel("j-never");
}

/// `signal_cancel` against an unknown job_id surfaces
/// `NotFound` — distinguishes "never started" from "already
/// finished" only by virtue of the FSM unregistering on tail.
#[test]
fn signal_cancel_unknown_returns_not_found() {
    let reg = JobRegistry::new();
    let err = reg
        .signal_cancel("j-nope")
        .expect_err("unknown rejected");
    assert_eq!(err.kind(), "not_found");
}

/// `signal_cancel` wakes a waiter on the registered Notify.
/// We register, await `notified()` from one task, signal from
/// another, and assert the waiter task observes the wake-up.
#[tokio::test]
async fn signal_cancel_wakes_registered_notify() {
    let reg = Arc::new(JobRegistry::new());
    let notify = Arc::new(tokio::sync::Notify::new());
    reg.register_cancel("j-w1", Arc::clone(&notify))
        .expect("register ok");

    let waiter_notify = Arc::clone(&notify);
    let waiter = tokio::spawn(async move {
        waiter_notify.notified().await;
    });

    // Give the waiter a tick to register its waker.
    tokio::task::yield_now().await;

    reg.signal_cancel("j-w1").expect("signal ok");
    // The wait must complete promptly; bound it so a regression
    // surfaces as a test failure rather than a hang.
    tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
        .await
        .expect("waiter did not wake within 1s")
        .expect("waiter task panicked");
}

/// Historical: the W4-06 FSM owned a `WorkspaceGuard` that called
/// `release_workspace` on Drop (both removed in W5-06 along with
/// fsm.rs). The release contract survives — callers invoke
/// `release_workspace` on success and failure paths — so this test
/// still exercises the double-release idempotency from an async
/// block.
#[tokio::test]
async fn try_acquire_workspace_releases_for_re_acquire() {
    let reg = JobRegistry::new();
    reg.try_acquire_workspace("ws-g", fixture_job("j-g"))
        .await
        .expect("acquire");
    reg.release_workspace("ws-g", "j-g").await;
    // Workspace is free — the next acquire wins.
    reg.try_acquire_workspace("ws-g", fixture_job("j-g2"))
        .await
        .expect("re-acquire after release");
}

// ---------------------------------------------------------------
// WP-W3-12b — `with_pool` smoke (in-memory only — exercising the
// pool-wired path lives in store.rs::tests + commands tests).
// ---------------------------------------------------------------

/// `with_pool` constructor wires the pool and `has_pool()`
/// returns true. `new()` returns false. Used by parameterized
/// FSM tests to assert the right backend is in use.
#[tokio::test]
async fn with_pool_constructor_records_pool_handle() {
    let (pool, _dir) = crate::test_support::fresh_pool().await;
    let reg = JobRegistry::with_pool(pool);
    assert!(reg.has_pool(), "with_pool wires the handle");
    let reg2 = JobRegistry::new();
    assert!(!reg2.has_pool(), "new() leaves pool unset");
}

// ---------------------------------------------------------------
// WP-W3-12e — last_rejecting_gate derivation
// ---------------------------------------------------------------

fn stage_with_verdict(state: JobState, approved: bool) -> StageResult {
    StageResult {
        state,
        specialist_id: format!("{state:?}").to_lowercase(),
        assistant_text: "x".into(),
        session_id: "sess".into(),
        total_cost_usd: 0.0,
        duration_ms: 0,
        verdict: Some(Verdict {
            approved,
            issues: Vec::new(),
            summary: "s".into(),
        }),
        coordinator_decision: None,
    }
}

/// Empty stages → no rejecting gate.
#[test]
fn last_rejecting_gate_empty_stages_returns_none() {
    let job = fixture_job("j-no-stages");
    assert!(job.last_rejecting_gate().is_none());
}

/// All-approved chain → no rejecting gate.
#[test]
fn last_rejecting_gate_all_approved_returns_none() {
    let mut job = fixture_job("j-all-ok");
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, true));
    assert!(job.last_rejecting_gate().is_none());
}

/// Reviewer rejected on the most recent attempt → returns Review.
#[test]
fn last_rejecting_gate_returns_review_when_review_rejected() {
    let mut job = fixture_job("j-rev");
    job.stages.push(stage_with_verdict(JobState::Review, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
}

/// Tester rejected after Reviewer approved → returns Test (the
/// most recent rejecting gate, NOT the most recent gate overall).
#[test]
fn last_rejecting_gate_returns_test_when_test_rejected() {
    let mut job = fixture_job("j-test");
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
}

/// Stages without verdicts (Scout/Plan/Build) are skipped.
#[test]
fn last_rejecting_gate_skips_non_verdict_stages() {
    let mut job = fixture_job("j-mix");
    // A Scout stage with `verdict=None` must not throw the helper
    // off; only Review/Test entries with rejected verdicts count.
    job.stages.push(StageResult {
        state: JobState::Scout,
        specialist_id: "scout".into(),
        assistant_text: "sc".into(),
        session_id: "s".into(),
        total_cost_usd: 0.0,
        duration_ms: 0,
        verdict: None,
        coordinator_decision: None,
    });
    job.stages.push(stage_with_verdict(JobState::Review, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Review));
}

/// Newest rejection wins — even if an earlier Review rejected,
/// the most recent rejecting gate is the one returned. This
/// matches the retry loop's intent: label the gate that just
/// triggered the upcoming retry, not an older one.
#[test]
fn last_rejecting_gate_returns_newest_rejection() {
    let mut job = fixture_job("j-newest");
    job.stages.push(stage_with_verdict(JobState::Review, false));
    // Retry round: Reviewer approved this time, Tester rejected.
    job.stages.push(stage_with_verdict(JobState::Plan, true)); // no verdict shape; helper ignores
    job.stages.push(stage_with_verdict(JobState::Review, true));
    job.stages.push(stage_with_verdict(JobState::Test, false));
    assert_eq!(job.last_rejecting_gate(), Some(JobState::Test));
}

/// `SwarmJobEvent::RetryStarted` serializes to the documented
/// snake_case wire shape with all fields present at the top level.
#[test]
fn swarm_job_event_retry_started_serializes() {
    let evt = SwarmJobEvent::RetryStarted {
        job_id: "j-1".into(),
        attempt: 2,
        max_retries: 2,
        triggered_by: JobState::Review,
        verdict: Verdict {
            approved: false,
            issues: Vec::new(),
            summary: "rejected".into(),
        },
    };
    let json = serde_json::to_value(&evt).expect("serialize");
    assert_eq!(
        json.get("kind").and_then(|v| v.as_str()),
        Some("retry_started")
    );
    assert_eq!(json.get("attempt").and_then(|v| v.as_u64()), Some(2));
    assert_eq!(
        json.get("max_retries").and_then(|v| v.as_u64()),
        Some(2)
    );
    assert_eq!(
        json.get("triggered_by").and_then(|v| v.as_str()),
        Some("review"),
        "triggered_by uses JobState's snake_case wire shape"
    );
    let verdict = json.get("verdict").expect("verdict embedded");
    assert_eq!(
        verdict.get("approved").and_then(|v| v.as_bool()),
        Some(false)
    );
}

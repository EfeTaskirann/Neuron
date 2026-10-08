//! `JobRegistry` tests — acquire / release / cancel surface, the
//! per-workspace lock semantics (WP-W3-12c), and the `with_pool`
//! smoke (WP-W3-12b).

use std::sync::Arc;
use tokio::sync::Barrier;

use super::super::{JobRegistry, JobState};
use super::fixture_job;

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

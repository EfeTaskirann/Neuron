// `super::*` already brings in `tauri::Manager` from the module's
// top-level imports, so `app.state::<...>()` resolves without an
// extra `use tauri::Manager as _` here.
use super::*;
use crate::test_support::{mock_app_with_pool, seed_minimal};

/// Seed a workflow + run for `runs:list/get/cancel` happy paths.
async fn seed_run(pool: &crate::db::DbPool, id: &str, status: &str) {
    seed_minimal(pool).await;
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES (?, 'w1', 'Daily summary', 1, ?)",
    )
    .bind(id)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn runs_list_empty_returns_empty_vec() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let out = runs_list(state, None).await.expect("ok");
    assert!(out.is_empty());
}

#[tokio::test]
async fn runs_list_filters_by_status() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_minimal(&pool).await;
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) VALUES \
         ('r1','w1','Daily summary',1,'success'), \
         ('r2','w1','Daily summary',2,'running'), \
         ('r3','w1','Daily summary',3,'error')",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let out = runs_list(
        state,
        Some(RunFilter {
            status: Some("running".into()),
            workflow_id: None,
        }),
    )
    .await
    .expect("ok");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].status, "running");
}

#[tokio::test]
async fn runs_get_returns_run_and_spans() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "running").await;
    sqlx::query(
        "INSERT INTO runs_spans (id, run_id, name, type, t0_ms) \
         VALUES ('s1','r1','plan','llm',100)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let detail = runs_get(state, "r1".to_string()).await.expect("ok");
    assert_eq!(detail.run.id, "r1");
    assert_eq!(detail.spans.len(), 1);
    assert_eq!(detail.spans[0].span_type, "llm");
}

#[tokio::test]
async fn runs_get_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let err = runs_get(state, "nope".to_string()).await.unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

/// When the LangGraph sidecar is not in app state (no Python,
/// unsynced venv, mock-runtime tests), `runs:create` finalises
/// the freshly-inserted row to `status='error'` and surfaces a
/// `Sidecar` error to the caller. This replaces the prior
/// behaviour of leaving a phantom `running` row that never
/// finalised — see report.md §K2.
#[tokio::test]
async fn runs_create_without_sidecar_marks_row_error() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_minimal(&pool).await;
    let state = app.state::<crate::db::DbPool>();

    let err = runs_create(app.handle().clone(), state, "w1".to_string())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "sidecar");

    // DB side-effect: row exists with `status='error'` and a
    // populated `duration_ms`. No spans are linked because we
    // never dispatched to the sidecar.
    let (count_runs, error_count, running_count): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
            (SELECT COUNT(*) FROM runs), \
            (SELECT COUNT(*) FROM runs WHERE status='error'), \
            (SELECT COUNT(*) FROM runs WHERE status='running')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count_runs, 1);
    assert_eq!(error_count, 1, "row must be finalised to error");
    assert_eq!(running_count, 0, "no zombie running rows allowed");

    let count_spans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs_spans")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count_spans, 0);
}

#[tokio::test]
async fn runs_create_unknown_workflow_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let err = runs_create(app.handle().clone(), state, "no-such".into())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn runs_cancel_transitions_running_to_cancelled() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "running").await;
    let state = app.state::<crate::db::DbPool>();
    runs_cancel(state, "r1".into()).await.expect("ok");
    let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
        .bind("r1")
        .fetch_one(&pool)
        .await
        .unwrap();
    // Y16: distinguish user cancel from sidecar error in the runs
    // list. Schema CHECK now allows 'cancelled' as a 4th terminal
    // state.
    assert_eq!(status, "cancelled");
}

/// K3 regression: a run that is already `cancelled` (or any
/// non-running state) cannot be re-cancelled — the late
/// `RunCompleted` event's `finalise_run` UPDATE is gated by
/// `WHERE status = 'running'`, so it cannot ezme the cancel.
/// `runs:cancel` itself uses the same atomic gate.
#[tokio::test]
async fn runs_cancel_on_cancelled_run_is_conflict() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "cancelled").await;
    let state = app.state::<crate::db::DbPool>();
    let err = runs_cancel(state, "r1".into()).await.unwrap_err();
    assert_eq!(err.kind(), "conflict");
}

#[tokio::test]
async fn runs_cancel_already_done_is_conflict() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "success").await;
    let state = app.state::<crate::db::DbPool>();
    let err = runs_cancel(state, "r1".into()).await.unwrap_err();
    assert_eq!(err.kind(), "conflict");
}

/// WP-W2-07: `runs:get` walks the `parent_span_id` chain and tags
/// each span with its tree depth. A 3-level chain (root → child →
/// grandchild) yields indents 0, 1, 2.
#[tokio::test]
async fn runs_get_computes_indent_from_parent_tree() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "running").await;
    sqlx::query(
        "INSERT INTO runs_spans (id, run_id, parent_span_id, name, type, t0_ms) VALUES \
         ('s1','r1', NULL, 'orchestrator.run', 'logic', 100), \
         ('s2','r1', 's1',  'llm.plan',        'llm',   200), \
         ('s3','r1', 's2',  'logic.route',     'logic', 300)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let detail = runs_get(state, "r1".to_string()).await.expect("ok");
    let by_id: std::collections::HashMap<_, _> = detail
        .spans
        .iter()
        .map(|s| (s.id.as_str(), s.indent))
        .collect();
    assert_eq!(by_id["s1"], 0, "root indent");
    assert_eq!(by_id["s2"], 1, "child indent");
    assert_eq!(by_id["s3"], 2, "grandchild indent");
}

/// A span whose `parent_span_id` points to a row in a *different*
/// run satisfies the schema FK but lives outside this run's tree.
/// The CTE's `WHERE rs.run_id = ?1` predicate prevents traversal
/// from picking it up, so the LEFT JOIN + COALESCE must keep the
/// span in the result with indent 0 rather than dropping it.
#[tokio::test]
async fn runs_get_orphan_span_gets_indent_zero() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "running").await;
    // Sibling run with its own span, which becomes an
    // out-of-tree parent target for r1's orphan.
    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, status) \
         VALUES ('r2','w1','Daily summary',2,'running')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs_spans (id, run_id, parent_span_id, name, type, t0_ms) VALUES \
         ('s-far','r2', NULL, 'far-root', 'logic', 50), \
         ('s-orphan','r1', 's-far', 'orphaned-into-r2', 'llm', 100)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let detail = runs_get(state, "r1".to_string()).await.expect("ok");
    assert_eq!(detail.spans.len(), 1, "must not include r2's span");
    assert_eq!(detail.spans[0].id, "s-orphan");
    assert_eq!(
        detail.spans[0].indent, 0,
        "out-of-run parent → indent 0 (LEFT JOIN COALESCE), not dropped"
    );
}

/// Two sibling roots both get indent 0; their respective children
/// both get indent 1. Confirms the CTE handles forests, not just
/// single trees.
#[tokio::test]
async fn runs_get_two_root_spans_both_indent_zero() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    seed_run(&pool, "r1", "running").await;
    sqlx::query(
        "INSERT INTO runs_spans (id, run_id, parent_span_id, name, type, t0_ms) VALUES \
         ('a','r1', NULL, 'root-a',  'logic', 10), \
         ('b','r1', NULL, 'root-b',  'logic', 20), \
         ('a1','r1','a',  'child-a', 'llm',   30), \
         ('b1','r1','b',  'child-b', 'llm',   40)",
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = app.state::<crate::db::DbPool>();
    let detail = runs_get(state, "r1".to_string()).await.expect("ok");
    let by_id: std::collections::HashMap<_, _> = detail
        .spans
        .iter()
        .map(|s| (s.id.as_str(), s.indent))
        .collect();
    assert_eq!(by_id["a"], 0);
    assert_eq!(by_id["b"], 0);
    assert_eq!(by_id["a1"], 1);
    assert_eq!(by_id["b1"], 1);
}

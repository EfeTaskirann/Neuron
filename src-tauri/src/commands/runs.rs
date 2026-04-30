//! `runs:*` namespace.
//!
//! - `runs:list`   `(filter?)` → `Run[]`
//! - `runs:get`    `(id)` → `RunDetail` (run + spans)
//! - `runs:create` `(workflowId)` → `Run`        // WP-04 — real LangGraph execution
//! - `runs:cancel` `(id)` → `void`
//!
//! ## WP-04 — real run execution
//!
//! `runs:create` now:
//!
//! 1. Validates the workflow exists.
//! 2. Inserts a `runs` row with `status='running'` (FK + CHECK).
//! 3. Posts a `run.start` frame to the LangGraph Python sidecar.
//! 4. Returns the `Run` immediately — span events arrive
//!    asynchronously via the sidecar's read loop, which writes them
//!    to `runs_spans` and emits `runs:{id}:span` Tauri events.
//!
//! `runs:cancel` flips a `running` row to `error` (cancellation is a
//! flavour of error in the schema's CHECK constraint). Cancel-mid-LLM
//! propagation through the sidecar is out of scope for WP-W2-04 per
//! its §"Out of scope"; Week 3 wires that.

use tauri::{AppHandle, Manager, Runtime, State};
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Run, RunDetail, RunFilter, Span};
use crate::sidecar::agent::SidecarHandle;
use crate::time::now_seconds;

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn runs_list(
    pool: State<'_, DbPool>,
    filter: Option<RunFilter>,
) -> Result<Vec<Run>, AppError> {
    let mut sql = String::from(
        "SELECT id, workflow_id, workflow_name, started_at, duration_ms, tokens, cost_usd, status \
         FROM runs",
    );
    let mut clauses = Vec::<&str>::new();
    let f = filter.unwrap_or_default();
    if f.status.is_some() {
        clauses.push("status = ?");
    }
    if f.workflow_id.is_some() {
        clauses.push("workflow_id = ?");
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY started_at DESC");

    let mut q = sqlx::query_as::<_, Run>(&sql);
    if let Some(s) = &f.status {
        q = q.bind(s);
    }
    if let Some(w) = &f.workflow_id {
        q = q.bind(w);
    }
    Ok(q.fetch_all(pool.inner()).await?)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn runs_get(pool: State<'_, DbPool>, id: String) -> Result<RunDetail, AppError> {
    let run = sqlx::query_as::<_, Run>(
        "SELECT id, workflow_id, workflow_name, started_at, duration_ms, tokens, cost_usd, status \
         FROM runs WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(pool.inner())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Run {id} not found")))?;

    // Indent is computed at read time per WP-W2-07 §"Notes" — a
    // `WITH RECURSIVE` walk from each root (parent_span_id IS NULL)
    // counts depth. The `LEFT JOIN` + `COALESCE(t.indent, 0)` handles
    // orphan spans whose parent_span_id points outside the tree (e.g.,
    // a sidecar emitted child before parent landed) without dropping
    // them from the result set. The `run_id` predicate inside the
    // recursive arm prevents traversal escaping into other runs.
    let spans = sqlx::query_as::<_, Span>(
        "WITH RECURSIVE span_tree(id, indent) AS ( \
            SELECT id, 0 FROM runs_spans \
                WHERE run_id = ?1 AND parent_span_id IS NULL \
            UNION ALL \
            SELECT rs.id, st.indent + 1 \
                FROM runs_spans rs \
                JOIN span_tree st ON rs.parent_span_id = st.id \
                WHERE rs.run_id = ?1 \
         ) \
         SELECT s.id, s.run_id, s.parent_span_id, s.name, s.type, \
                s.t0_ms, s.duration_ms, s.attrs_json, s.prompt, s.response, \
                s.is_running, COALESCE(t.indent, 0) AS indent \
         FROM runs_spans s \
         LEFT JOIN span_tree t ON t.id = s.id \
         WHERE s.run_id = ?1 \
         ORDER BY s.t0_ms",
    )
    .bind(&id)
    .fetch_all(pool.inner())
    .await?;

    Ok(RunDetail { run, spans })
}

/// Insert a `runs` row with `status='running'` and dispatch the run
/// to the LangGraph sidecar.
///
/// The sidecar handle is looked up via `AppHandle::try_state` rather
/// than as a `tauri::State` argument because `Option<State<...>>` is
/// not a `specta::Type` and the binding generator rejects it. Tests
/// (and CI runners without a synced Python venv) skip the dispatch
/// path naturally — `try_state::<SidecarHandle>` returns `None` and
/// the inserted run row is the only side-effect.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn runs_create<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    workflow_id: String,
) -> Result<Run, AppError> {
    // The workflow must exist — the runs table FK enforces this, but
    // surfacing a `NotFound` here is friendlier than a `DbError` from
    // the constraint.
    let workflow_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM workflows WHERE id = ?")
            .bind(&workflow_id)
            .fetch_optional(pool.inner())
            .await?;
    let workflow_name = workflow_name
        .ok_or_else(|| AppError::NotFound(format!("Workflow {workflow_id} not found")))?;

    let id = format!("r-{}", Ulid::new());
    let started_at = now_seconds();

    sqlx::query(
        "INSERT INTO runs (id, workflow_id, workflow_name, started_at, duration_ms, tokens, cost_usd, status) \
         VALUES (?, ?, ?, ?, NULL, 0, 0, 'running')",
    )
    .bind(&id)
    .bind(&workflow_id)
    .bind(&workflow_name)
    .bind(started_at)
    .execute(pool.inner())
    .await?;

    // Dispatch to the sidecar. Two distinct error paths:
    //
    // 1. Sidecar never came up at app start (`try_state` is `None`):
    //    Python isn't installed, the venv is unsynced, etc. The user
    //    cannot do anything about this run, so we mark it `error`
    //    immediately rather than leaving a phantom `running` row that
    //    never finalises.
    // 2. `start_run` write fails (broken pipe — child died between
    //    spawn and now): same outcome — finalise to `error` and surface
    //    the underlying error, so the runs list does not stay polluted
    //    with zombie `running` rows on every failure.
    let sidecar_result = match app.try_state::<SidecarHandle>() {
        Some(handle) => handle.start_run(&workflow_id, &id).await,
        None => Err(AppError::Sidecar(
            "agent runtime sidecar is not running (run `cd src-tauri/sidecar/agent_runtime && uv sync`)".into(),
        )),
    };
    if let Err(e) = sidecar_result {
        // Compensating rollback: flip the freshly-inserted `running`
        // row to `error` atomically. The helper preserves the
        // `WHERE status = 'running'` guard so a sidecar-driven
        // success/error finalisation that already landed cannot be
        // overwritten.
        let _ = crate::commands::util::finalise_run_with(pool.inner(), &id, "error").await;
        return Err(e);
    }

    Ok(Run {
        id,
        workflow_name,
        workflow_id,
        started_at,
        duration_ms: None,
        tokens: 0,
        cost_usd: 0.0,
        status: "running".into(),
    })
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn runs_cancel(pool: State<'_, DbPool>, id: String) -> Result<(), AppError> {
    // Atomic conditional flip: only a `running` row transitions to
    // `cancelled`. Everything else (including `cancelled` itself) is a
    // conflict. Using a single `UPDATE … WHERE status='running'`
    // closes the TOCTOU window between SELECT and UPDATE that allowed
    // the sidecar's `finalise_run` to ezme a just-issued cancel —
    // see report.md §K3.
    let result = sqlx::query(
        "UPDATE runs SET status = 'cancelled' \
         WHERE id = ? AND status = 'running'",
    )
    .bind(&id)
    .execute(pool.inner())
    .await?;
    if result.rows_affected() == 1 {
        return Ok(());
    }
    // No row flipped: either the run does not exist or it is already
    // in a terminal state. Disambiguate with one extra read so the
    // caller gets a precise error.
    let existing: Option<(String,)> = sqlx::query_as("SELECT status FROM runs WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool.inner())
        .await?;
    match existing {
        None => Err(AppError::NotFound(format!("Run {id} not found"))),
        Some((status,)) => Err(AppError::Conflict(format!(
            "Run {id} is {status}, not running"
        ))),
    }
}


#[cfg(test)]
mod tests {
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
}

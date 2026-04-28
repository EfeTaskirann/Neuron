//! `runs:*` namespace.
//!
//! - `runs:list`   `(filter?)` → `Run[]`
//! - `runs:get`    `(id)` → `RunDetail` (run + spans)
//! - `runs:create` `(workflowId)` → `Run`        // STUB — inserts row, no execution
//! - `runs:cancel` `(id)` → `void`
//!
//! ## Stubs
//!
//! WP-W2-03 § "Stubs in this WP":
//!
//!   `runs:create` inserts a row with `status='running'` and no spans.
//!   WP-04 makes it real.
//!
//! `runs:cancel` flips a `running` row to `error` (cancellation is a
//! flavour of error in the schema's CHECK constraint). The "real"
//! cancellation that propagates into the LangGraph runtime is in WP-04.

use tauri::State;
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Run, RunDetail, RunFilter, Span};

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

    let spans = sqlx::query_as::<_, Span>(
        "SELECT id, run_id, parent_span_id, name, type, t0_ms, duration_ms, attrs_json, prompt, response, is_running \
         FROM runs_spans WHERE run_id = ? ORDER BY t0_ms",
    )
    .bind(&id)
    .fetch_all(pool.inner())
    .await?;

    Ok(RunDetail { run, spans })
}

/// **STUB.** Inserts a row in `runs` with `status='running'` and no
/// spans, returns the inserted row. Real execution lands in WP-04.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn runs_create(
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
    // Only `running` rows are cancellable; anything else is a conflict.
    let row: Option<(String,)> = sqlx::query_as("SELECT status FROM runs WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool.inner())
        .await?;
    let status = row
        .ok_or_else(|| AppError::NotFound(format!("Run {id} not found")))?
        .0;
    if status != "running" {
        return Err(AppError::Conflict(format!(
            "Run {id} is {status}, not running"
        )));
    }
    sqlx::query("UPDATE runs SET status = 'error' WHERE id = ?")
        .bind(&id)
        .execute(pool.inner())
        .await?;
    Ok(())
}

fn now_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{fresh_pool, seed_minimal};
    use tauri::Manager as _;

    async fn mock_app_with_pool() -> (
        tauri::App<tauri::test::MockRuntime>,
        crate::db::DbPool,
        tempfile::TempDir,
    ) {
        let (pool, dir) = fresh_pool().await;
        let app = tauri::test::mock_builder()
            .manage(pool.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        (app, pool, dir)
    }

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

    #[tokio::test]
    async fn runs_create_inserts_running_row_with_no_spans() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_minimal(&pool).await;
        let state = app.state::<crate::db::DbPool>();

        let run = runs_create(state, "w1".to_string()).await.expect("ok");
        assert_eq!(run.status, "running");
        assert_eq!(run.workflow_name, "Daily summary");
        assert_eq!(run.tokens, 0);
        assert!(run.duration_ms.is_none());
        assert!(run.id.starts_with("r-"));

        // DB side-effect: row exists, no spans linked.
        let count_runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        let count_spans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs_spans")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count_runs, 1);
        assert_eq!(count_spans, 0);
    }

    #[tokio::test]
    async fn runs_create_unknown_workflow_is_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let err = runs_create(state, "no-such".into()).await.unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }

    #[tokio::test]
    async fn runs_cancel_transitions_running_to_error() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_run(&pool, "r1", "running").await;
        let state = app.state::<crate::db::DbPool>();
        runs_cancel(state, "r1".into()).await.expect("ok");
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
            .bind("r1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "error");
    }

    #[tokio::test]
    async fn runs_cancel_already_done_is_conflict() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        seed_run(&pool, "r1", "success").await;
        let state = app.state::<crate::db::DbPool>();
        let err = runs_cancel(state, "r1".into()).await.unwrap_err();
        assert_eq!(err.kind(), "conflict");
    }
}

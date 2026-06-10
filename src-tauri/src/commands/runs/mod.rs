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
mod tests;

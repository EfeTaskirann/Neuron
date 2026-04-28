//! `workflows:*` namespace.
//!
//! - `workflows:list` → `Workflow[]`
//! - `workflows:get`  `(id)` → `WorkflowDetail` (workflow + nodes + edges)

use tauri::State;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Edge, Node, Workflow, WorkflowDetail};

#[tauri::command]
#[specta::specta]
pub async fn workflows_list(pool: State<'_, DbPool>) -> Result<Vec<Workflow>, AppError> {
    let rows = sqlx::query_as::<_, Workflow>(
        "SELECT id, name, saved_at FROM workflows ORDER BY saved_at DESC",
    )
    .fetch_all(pool.inner())
    .await?;
    Ok(rows)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn workflows_get(
    pool: State<'_, DbPool>,
    id: String,
) -> Result<WorkflowDetail, AppError> {
    let workflow = sqlx::query_as::<_, Workflow>(
        "SELECT id, name, saved_at FROM workflows WHERE id = ?",
    )
    .bind(&id)
    .fetch_optional(pool.inner())
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Workflow {id} not found")))?;

    let nodes = sqlx::query_as::<_, Node>(
        "SELECT id, workflow_id, kind, x, y, title, meta, status \
         FROM nodes WHERE workflow_id = ? ORDER BY id",
    )
    .bind(&id)
    .fetch_all(pool.inner())
    .await?;

    let edges = sqlx::query_as::<_, Edge>(
        "SELECT id, workflow_id, from_node, to_node, active \
         FROM edges WHERE workflow_id = ? ORDER BY id",
    )
    .bind(&id)
    .fetch_all(pool.inner())
    .await?;

    Ok(WorkflowDetail {
        workflow,
        nodes,
        edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fresh_pool;
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

    #[tokio::test]
    async fn workflows_list_empty_returns_empty_vec() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let out = workflows_list(state).await.expect("ok");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn workflows_list_returns_seeded_rows() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool)
            .await
            .unwrap();
        let state = app.state::<crate::db::DbPool>();
        let out = workflows_list(state).await.expect("ok");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Daily summary");
    }

    #[tokio::test]
    async fn workflows_get_returns_detail_with_nodes_and_edges() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, workflow_id, kind, x, y, title, meta) \
             VALUES ('n1','w1','llm',10,20,'Plan','{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO nodes (id, workflow_id, kind, x, y, title, meta) \
             VALUES ('n2','w1','tool',30,40,'Search','{}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO edges (id, workflow_id, from_node, to_node, active) \
             VALUES ('e1','w1','n1','n2',1)",
        )
        .execute(&pool)
        .await
        .unwrap();

        let state = app.state::<crate::db::DbPool>();
        let detail = workflows_get(state, "w1".to_string()).await.expect("ok");
        assert_eq!(detail.workflow.id, "w1");
        assert_eq!(detail.nodes.len(), 2);
        assert_eq!(detail.edges.len(), 1);
        assert!(detail.edges[0].active);
    }

    #[tokio::test]
    async fn workflows_get_unknown_id_is_not_found() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let err = workflows_get(state, "nope".to_string()).await.unwrap_err();
        assert_eq!(err.kind(), "not_found");
    }
}

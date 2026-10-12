use crate::error::AppError;
use crate::models::{AgentCreateInput, AgentPatch};
use crate::test_support::mock_app_with_pool;
// `app.state::<DbPool>()` is a `Manager`-trait method; bring the
// trait in scope so the tests resolve it. Aliased to `_` so we
// don't trip the unused-import lint on test-only builds.
use tauri::Manager as _;

use super::*;

#[tokio::test]
async fn agents_list_empty_returns_empty_vec() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let out = agents_list(state).await.expect("ok");
    assert!(out.is_empty());
}

#[tokio::test]
async fn agents_list_returns_seeded_rows() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query(
        "INSERT INTO agents (id, name, model, temp, role) VALUES \
         ('a1','Planner','gpt-4o',0.4,'r1'), \
         ('a2','Reasoner','claude',0.3,'r2')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let state = app.state::<crate::db::DbPool>();
    let out = agents_list(state).await.expect("ok");
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].name, "Planner");
    assert_eq!(out[1].name, "Reasoner");
}

#[tokio::test]
async fn agents_get_returns_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query("INSERT INTO agents VALUES ('a1','X','m',0.5,'r',0)")
        .execute(&pool)
        .await
        .unwrap();
    let state = app.state::<crate::db::DbPool>();
    let agent = agents_get(state, "a1".to_string()).await.expect("ok");
    assert_eq!(agent.id, "a1");
}

#[tokio::test]
async fn agents_get_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let state = app.state::<crate::db::DbPool>();
    let err = agents_get(state, "nope".to_string()).await.unwrap_err();
    assert_eq!(err.kind(), "not_found");
    assert!(err.message().contains("nope"));
}

#[tokio::test]
async fn agents_create_inserts_and_returns_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let res = agents_create(
        handle,
        state,
        AgentCreateInput {
            name: "Reviewer".into(),
            model: "claude-sonnet-4-6".into(),
            temp: 0.2,
            role: "Critiques drafts".into(),
        },
    )
    .await
    .expect("ok");
    assert_eq!(res.name, "Reviewer");
    assert_eq!(res.id.len(), 26, "ULID is 26 chars");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn agents_create_rejects_empty_name() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = agents_create(
        handle,
        state,
        AgentCreateInput {
            name: "  ".into(),
            model: "gpt-4o".into(),
            temp: 0.5,
            role: "x".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "invalid_input");
    assert!(matches!(err, AppError::InvalidInput(_)));
}

#[tokio::test]
async fn agents_update_writes_patched_fields() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query("INSERT INTO agents VALUES ('a1','X','m',0.5,'r',0)")
        .execute(&pool)
        .await
        .unwrap();
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let res = agents_update(
        handle,
        state,
        "a1".to_string(),
        AgentPatch {
            name: Some("Y".into()),
            ..Default::default()
        },
    )
    .await
    .expect("ok");
    assert_eq!(res.name, "Y");
}

#[tokio::test]
async fn agents_update_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = agents_update(
        handle,
        state,
        "nope".to_string(),
        AgentPatch {
            name: Some("Y".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

#[tokio::test]
async fn agents_delete_removes_row() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    sqlx::query("INSERT INTO agents VALUES ('a1','X','m',0.5,'r',0)")
        .execute(&pool)
        .await
        .unwrap();
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    agents_delete(handle, state, "a1".to_string())
        .await
        .expect("ok");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn agents_delete_unknown_id_is_not_found() {
    let (app, _pool, _dir) = mock_app_with_pool().await;
    let handle = app.handle().clone();
    let state = app.state::<crate::db::DbPool>();
    let err = agents_delete(handle, state, "nope".to_string())
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "not_found");
}

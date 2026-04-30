//! `me:*` namespace.
//!
//! - `me:get` `()` → `Me`
//!
//! Week 2 returns hardcoded user + workspace count from `workflows`.
//! Week 3 will source the user from a settings table; the wire shape
//! does not change.

use tauri::State;

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Me, User, Workspace};

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn me_get(pool: State<'_, DbPool>) -> Result<Me, AppError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflows")
        .fetch_one(pool.inner())
        .await?;
    Ok(Me {
        user: User {
            initials: "ET".into(),
            name: "Efe Taşkıran".into(),
        },
        workspace: Workspace {
            name: "Personal".into(),
            count,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use tauri::Manager as _;

    #[tokio::test]
    async fn me_get_returns_hardcoded_user_and_workspace_count() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w1','Daily summary')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO workflows (id, name) VALUES ('w2','PR review')")
            .execute(&pool)
            .await
            .unwrap();

        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.user.initials, "ET");
        assert_eq!(me.user.name, "Efe Taşkıran");
        assert_eq!(me.workspace.name, "Personal");
        assert_eq!(me.workspace.count, 2);
    }

    #[tokio::test]
    async fn me_get_with_empty_db_returns_zero_count() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.workspace.count, 0);
    }
}

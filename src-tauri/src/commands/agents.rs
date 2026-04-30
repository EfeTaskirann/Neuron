//! `agents:*` namespace.
//!
//! Per the WP-W2-03 command list:
//!
//! - `agents:list`   → `Agent[]`
//! - `agents:get`    `(id)` → `Agent`
//! - `agents:create` `(input)` → `Agent`
//! - `agents:update` `(id, patch)` → `Agent`
//! - `agents:delete` `(id)` → `void`
//!
//! ## IPC name deviation
//!
//! Tauri 2 does not expose a `name = "..."` argument on
//! `#[tauri::command]` (only `rename_all`, `root`, `async`), and the
//! invoke-handler dispatches against the literal Rust function
//! identifier. We therefore register snake_case names (`agents_list`,
//! `agents_get`, …) on the IPC and let `tauri-specta` emit the
//! camelCase frontend façade (`commands.agentsList()` etc.) into
//! `bindings.ts`. The colon-namespaced form in the WP body refers to
//! the *logical* namespace, not a literal IPC string. See
//! `lib.rs` and `AGENT_LOG.md` for the deviation note.
//!
//! ## `agents:changed` event
//!
//! ADR-0006 reserves a single coalesced `agents.changed` event for
//! create/update/delete. Tauri 2.10 rejects `.` in event names so the
//! wire form is `agents:changed` (the colon is allowed and matches the
//! command-surface convention). Each mutator emits it with payload
//! `{ id, op }`. WP-W2-08's `useAgents` will subscribe and invalidate
//! the `['agents']` query.

use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime, State};
use ulid::Ulid;

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::{Agent, AgentCreateInput, AgentPatch};

/// Coalesced change payload for the `agents.changed` event.
#[derive(Debug, Serialize, specta::Type, Clone)]
#[serde(rename_all = "camelCase")]
struct AgentChanged<'a> {
    id: &'a str,
    op: &'a str,
}

/// Single source of truth for the column list selected by every
/// `agents:*` read path. Keeping this as a `const` (rather than three
/// inlined SQL strings) means a future schema change touches the
/// projection in exactly one place — and the same list is reused by
/// `agents:update`'s `RETURNING` clause so the UPDATE-then-RETURN
/// path can never drift away from the SELECT path.
const AGENTS_COLS: &str = "id, name, model, temp, role";

// Generic over `R: Runtime` so unit tests can drive the same code path
// with `tauri::test::MockRuntime`. The IPC handler instantiates this
// with `tauri::Wry`; the macro re-exports the concrete name unchanged.
//
// Tauri 2.10 rejects `.` in event names (panics with `IllegalEventName`),
// so we use the `:` separator that is allowed and that matches the
// command-surface naming convention (`agents:list`, etc.). ADR-0006's
// `agents.changed` reads as `agents:changed` on the wire; the logical
// shape `{domain}.{verb}` is preserved by the WP-W2-08 frontend hooks
// when they subscribe via the `commands` façade.
fn emit_changed<R: Runtime>(app: &AppHandle<R>, id: &str, op: &str) -> Result<(), AppError> {
    app.emit(events::AGENTS_CHANGED, AgentChanged { id, op })
        .map_err(AppError::from)
}

#[tauri::command]
#[specta::specta]
pub async fn agents_list(pool: State<'_, DbPool>) -> Result<Vec<Agent>, AppError> {
    let sql = format!("SELECT {AGENTS_COLS} FROM agents ORDER BY name COLLATE NOCASE");
    let rows = sqlx::query_as::<_, Agent>(&sql)
        .fetch_all(pool.inner())
        .await?;
    Ok(rows)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn agents_get(pool: State<'_, DbPool>, id: String) -> Result<Agent, AppError> {
    let sql = format!("SELECT {AGENTS_COLS} FROM agents WHERE id = ?");
    let agent = sqlx::query_as::<_, Agent>(&sql)
        .bind(&id)
        .fetch_optional(pool.inner())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Agent {id} not found")))?;
    Ok(agent)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn agents_create<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    input: AgentCreateInput,
) -> Result<Agent, AppError> {
    if input.name.trim().is_empty() {
        return Err(AppError::InvalidInput("name must not be empty".into()));
    }
    if !(0.0..=2.0).contains(&input.temp) {
        return Err(AppError::InvalidInput(format!(
            "temp {} out of range [0.0, 2.0]",
            input.temp
        )));
    }

    let id = Ulid::new().to_string();
    sqlx::query(
        "INSERT INTO agents (id, name, model, temp, role) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&input.name)
    .bind(&input.model)
    .bind(input.temp)
    .bind(&input.role)
    .execute(pool.inner())
    .await?;

    emit_changed(&app, &id, "created")?;

    Ok(Agent {
        id,
        name: input.name,
        model: input.model,
        temp: input.temp,
        role: input.role,
    })
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn agents_update<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
    patch: AgentPatch,
) -> Result<Agent, AppError> {
    // Build a dynamic UPDATE that only writes provided fields. Empty
    // patches are explicitly rejected — silently no-op'ing on an empty
    // body would mask wiring bugs (frontend forgot to send the diff).
    let mut sets = Vec::<&str>::new();
    if patch.name.is_some() {
        sets.push("name = ?");
    }
    if patch.model.is_some() {
        sets.push("model = ?");
    }
    if patch.temp.is_some() {
        sets.push("temp = ?");
    }
    if patch.role.is_some() {
        sets.push("role = ?");
    }
    if sets.is_empty() {
        return Err(AppError::InvalidInput("patch is empty".into()));
    }
    if let Some(t) = patch.temp {
        if !(0.0..=2.0).contains(&t) {
            return Err(AppError::InvalidInput(format!(
                "temp {t} out of range [0.0, 2.0]"
            )));
        }
    }

    let sql = format!(
        "UPDATE agents SET {} WHERE id = ? RETURNING {AGENTS_COLS}",
        sets.join(", ")
    );
    let mut q = sqlx::query_as::<_, Agent>(&sql);
    if let Some(v) = &patch.name {
        q = q.bind(v);
    }
    if let Some(v) = &patch.model {
        q = q.bind(v);
    }
    if let Some(v) = patch.temp {
        q = q.bind(v);
    }
    if let Some(v) = &patch.role {
        q = q.bind(v);
    }
    q = q.bind(&id);

    let updated = q
        .fetch_optional(pool.inner())
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Agent {id} not found")))?;

    emit_changed(&app, &id, "updated")?;
    Ok(updated)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn agents_delete<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    id: String,
) -> Result<(), AppError> {
    let res = sqlx::query("DELETE FROM agents WHERE id = ?")
        .bind(&id)
        .execute(pool.inner())
        .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Agent {id} not found")));
    }
    emit_changed(&app, &id, "deleted")?;
    Ok(())
}

#[cfg(test)]
mod tests {
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
}

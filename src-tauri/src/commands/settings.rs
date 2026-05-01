//! `settings:*` namespace — key/value persistence (WP-W3-01).
//!
//! Backs the `me:get` user/workspace fields and the W3-08-era
//! Settings route's "Advanced" panel. **Never** touches the OS
//! keychain — secrets live in `crate::secrets` and have a separate
//! command surface (`secrets:*`). Keeping the two surfaces split
//! is a deliberate guarantee of WP-W3-01: a SQLite leak (e.g. an
//! exported backup file) never exposes credentials.
//!
//! - `settings:get(key)` → `Option<String>`
//! - `settings:set(key, value)` → `()` — empty value rejected
//!   (use `delete` for absence; matches the keychain semantics on
//!   the secrets side).
//! - `settings:delete(key)` → `()` — idempotent.
//! - `settings:list()` → `Vec<(String, String)>` — every row;
//!   used by the future Settings "Advanced" panel.
//!
//! Naming convention: keys are dot-namespaced
//! (`user.name`, `workspace.name`, future `otel.endpoint`,
//! `theme.mode`). The frontend can group rows by prefix without
//! a separate categorisation column.

use tauri::State;

use crate::db::DbPool;
use crate::error::AppError;

/// Internal helper used by `commands::me::me_get`. Plain
/// `SELECT value FROM settings WHERE key = ?`. Returns `Ok(None)`
/// when the row does not exist.
///
/// Kept as a separate function (rather than calling `settings_get`
/// the Tauri command from inside `me_get`) so non-IPC callers
/// don't have to fabricate a `tauri::State`.
pub async fn read(pool: &DbPool, key: &str) -> Result<Option<String>, AppError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn settings_get(
    pool: State<'_, DbPool>,
    key: String,
) -> Result<Option<String>, AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    read(pool.inner(), &key).await
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn settings_set(
    pool: State<'_, DbPool>,
    key: String,
    value: String,
) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    if value.is_empty() {
        return Err(AppError::InvalidInput(
            "value must not be empty (use settings:delete to clear)".into(),
        ));
    }
    sqlx::query(
        "INSERT INTO settings (key, value, updated_at) \
         VALUES (?, ?, CAST(strftime('%s','now') AS INTEGER)) \
         ON CONFLICT(key) DO UPDATE SET \
            value = excluded.value, \
            updated_at = CAST(strftime('%s','now') AS INTEGER)",
    )
    .bind(&key)
    .bind(&value)
    .execute(pool.inner())
    .await?;
    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn settings_delete(
    pool: State<'_, DbPool>,
    key: String,
) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return Err(AppError::InvalidInput("key must not be empty".into()));
    }
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(&key)
        .execute(pool.inner())
        .await?;
    Ok(())
}

/// Return every setting as `(key, value)` pairs sorted by key.
/// Used by the W3-08 Settings route's "Advanced" panel; the
/// updated_at column is intentionally omitted from the wire shape
/// to keep the typegen surface trivial.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn settings_list(
    pool: State<'_, DbPool>,
) -> Result<Vec<(String, String)>, AppError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(pool.inner())
            .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use tauri::Manager as _;

    /// Acceptance: the `0004_settings.sql` seed inserts the three
    /// rows needed by `me:get` (`user.name`, `user.initials`,
    /// `workspace.name`). `list()` returns ≥3 rows on a fresh DB.
    #[tokio::test]
    async fn list_returns_at_least_three_seeded_rows() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let rows = settings_list(state).await.expect("ok");
        assert!(rows.len() >= 3, "fresh db has ≥3 seeded rows; got {}", rows.len());
        let keys: Vec<&str> = rows.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"user.name"));
        assert!(keys.contains(&"user.initials"));
        assert!(keys.contains(&"workspace.name"));
    }

    /// get(key) returns the seeded value verbatim.
    #[tokio::test]
    async fn get_returns_seeded_value() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let got = settings_get(state, "user.name".into())
            .await
            .expect("ok");
        assert_eq!(got.as_deref(), Some("Efe Taşkıran"));
    }

    /// get on an unknown key yields `None` rather than erroring —
    /// callers use the result directly with `unwrap_or_else`
    /// (see `commands::me::me_get`).
    #[tokio::test]
    async fn get_unknown_key_returns_none() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let got = settings_get(state, "no.such.key".into())
            .await
            .expect("ok");
        assert!(got.is_none());
    }

    /// set inserts a fresh row, then overwrites it on conflict —
    /// proves the `ON CONFLICT(key) DO UPDATE` clause is wired.
    #[tokio::test]
    async fn set_inserts_then_updates_on_conflict() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();

        settings_set(state, "theme.mode".into(), "dark".into())
            .await
            .expect("first set");
        let v: Option<String> = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key='theme.mode'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(v.as_deref(), Some("dark"));

        let state = app.state::<DbPool>();
        settings_set(state, "theme.mode".into(), "light".into())
            .await
            .expect("second set");
        let v: Option<String> = sqlx::query_scalar(
            "SELECT value FROM settings WHERE key='theme.mode'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert_eq!(v.as_deref(), Some("light"), "ON CONFLICT must update");
    }

    /// set rejects an empty value: the schema enforces NOT NULL,
    /// but the command wraps the error so callers see
    /// `invalid_input` rather than `db_error`.
    #[tokio::test]
    async fn set_rejects_empty_value() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let err = settings_set(state, "user.name".into(), "".into())
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// set rejects an empty key with the same `invalid_input`
    /// error variant.
    #[tokio::test]
    async fn set_rejects_empty_key() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let err = settings_set(state, "".into(), "x".into())
            .await
            .expect_err("empty key rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// delete clears the row; subsequent get yields None. Running
    /// delete twice on the same key is a no-op (idempotent).
    #[tokio::test]
    async fn delete_is_idempotent() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        settings_delete(state, "user.name".into())
            .await
            .expect("first delete");
        let state = app.state::<DbPool>();
        settings_delete(state, "user.name".into())
            .await
            .expect("second delete is a no-op");
        let state = app.state::<DbPool>();
        let got = settings_get(state, "user.name".into())
            .await
            .expect("ok");
        assert!(got.is_none(), "delete cleared the row");
    }

    /// Full round-trip: set → get → delete → get(None). Plus a
    /// brand-new key (never seeded) to prove insert path.
    #[tokio::test]
    async fn round_trip_for_new_key() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        settings_set(state, "otel.endpoint".into(), "http://localhost:4318".into())
            .await
            .expect("set");
        let state = app.state::<DbPool>();
        let got = settings_get(state, "otel.endpoint".into())
            .await
            .expect("get");
        assert_eq!(got.as_deref(), Some("http://localhost:4318"));
        let state = app.state::<DbPool>();
        settings_delete(state, "otel.endpoint".into())
            .await
            .expect("delete");
        let state = app.state::<DbPool>();
        let got = settings_get(state, "otel.endpoint".into())
            .await
            .expect("get after delete");
        assert!(got.is_none());
    }

    /// Internal helper `read` is called by `commands::me`; it must
    /// produce identical results to the IPC command. Smoke test
    /// guards against the helper drifting.
    #[tokio::test]
    async fn helper_and_command_return_same_value() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<DbPool>();
        let from_cmd = settings_get(state, "user.initials".into())
            .await
            .expect("ok");
        let from_helper = read(&pool, "user.initials").await.expect("ok");
        assert_eq!(from_cmd, from_helper);
        assert_eq!(from_cmd.as_deref(), Some("ET"));
    }
}

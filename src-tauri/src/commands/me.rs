//! `me:*` namespace.
//!
//! - `me:get` `()` → `Me`
//!
//! WP-W2-03 returned hardcoded user + workspace name, with the
//! workspace count derived from `SELECT COUNT(*) FROM workflows`.
//! WP-W3-01 swapped the hardcoded strings out for reads against the
//! `settings` table (seeded from migration `0004_settings.sql`).
//! The wire shape (`Me { user, workspace }`) is unchanged so the
//! frontend `useMe()` hook continues to work without revision.
//!
//! Defaults: if a row is missing entirely (e.g. a user manually
//! deleted `user.name` via `settings:delete`) we fall back to a
//! sensible literal — `"User"` for the name, `"Personal"` for the
//! workspace, and an auto-derived `initials` slug for the user
//! avatar. The defaults exist so a partly-cleared `settings` table
//! never crashes the home shell; they're not advertised in user
//! docs.

use tauri::State;

use crate::commands::settings;
use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{Me, User, Workspace};

/// Derive a 1–3 character initials slug from a display name.
/// First letter of each whitespace-split word, uppercased,
/// truncated to three. The fallback is used when a user has
/// updated `user.name` via the Settings route but not
/// `user.initials` — the avatar refreshes automatically rather
/// than displaying the stale seeded "ET".
fn derive_initials(name: &str) -> String {
    let mut out = String::with_capacity(3);
    for word in name.split_whitespace() {
        if let Some(c) = word.chars().next() {
            for upper in c.to_uppercase() {
                out.push(upper);
            }
            if out.chars().count() >= 3 {
                break;
            }
        }
    }
    if out.is_empty() {
        // Empty name (or all-whitespace) — fall back to the same
        // literal Week-2 used so the frontend never sees a blank
        // avatar bubble.
        "U".into()
    } else {
        // Cap at 3 chars (multi-byte safe).
        out.chars().take(3).collect()
    }
}

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn me_get(pool: State<'_, DbPool>) -> Result<Me, AppError> {
    let pool = pool.inner();

    let user_name = settings::read(pool, "user.name")
        .await?
        .unwrap_or_else(|| "User".into());
    let user_initials = settings::read(pool, "user.initials")
        .await?
        .unwrap_or_else(|| derive_initials(&user_name));
    let workspace_name = settings::read(pool, "workspace.name")
        .await?
        .unwrap_or_else(|| "Personal".into());

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflows")
        .fetch_one(pool)
        .await?;

    Ok(Me {
        user: User {
            initials: user_initials,
            name: user_name,
        },
        workspace: Workspace {
            name: workspace_name,
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
    async fn me_get_reads_seeded_user_and_workspace() {
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
        // Values match the `0004_settings.sql` seed.
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

    /// Acceptance: editing `user.name` via the settings command
    /// surface is reflected on the next `me:get`. Proves the
    /// Settings route's primary write path works end-to-end.
    #[tokio::test]
    async fn me_get_reflects_settings_set_for_user_name() {
        let (app, _pool, _dir) = mock_app_with_pool().await;

        let state = app.state::<crate::db::DbPool>();
        crate::commands::settings::settings_set(state, "user.name".into(), "Ada Lovelace".into())
            .await
            .expect("settings_set");

        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.user.name, "Ada Lovelace");
    }

    /// Defence in depth: deleting `user.name` falls back to the
    /// literal "User" rather than crashing or returning an empty
    /// string. Proves the `unwrap_or_else` defaults are wired.
    #[tokio::test]
    async fn me_get_falls_back_to_literal_when_user_name_missing() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        crate::commands::settings::settings_delete(state, "user.name".into())
            .await
            .expect("settings_delete");
        // Also drop initials so we exercise the derive_initials
        // path against the literal "User".
        let state = app.state::<crate::db::DbPool>();
        crate::commands::settings::settings_delete(state, "user.initials".into())
            .await
            .expect("settings_delete");

        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        assert_eq!(me.user.name, "User");
        assert_eq!(me.user.initials, "U");
    }

    /// `user.initials` is independent of `user.name`: editing the
    /// name does NOT auto-overwrite an explicit initials value.
    /// Catches a regression where me:get would compute initials
    /// from the new name even when the user had set a custom slug.
    #[tokio::test]
    async fn me_get_preserves_explicit_initials_when_name_changes() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        // Seed has user.initials='ET'. Change the name only.
        let state = app.state::<crate::db::DbPool>();
        crate::commands::settings::settings_set(state, "user.name".into(), "Ada Lovelace".into())
            .await
            .expect("settings_set");
        let state = app.state::<crate::db::DbPool>();
        let me = me_get(state).await.expect("ok");
        // Initials still the seeded 'ET' — derive_initials must
        // NOT win when the explicit row exists.
        assert_eq!(me.user.initials, "ET");
        assert_eq!(me.user.name, "Ada Lovelace");
    }

    /// Deriving initials from a one-word name returns one letter;
    /// from a two-word name returns two; cap at three regardless.
    #[test]
    fn derive_initials_truncates_at_three_words() {
        assert_eq!(derive_initials("Ada"), "A");
        assert_eq!(derive_initials("Ada Lovelace"), "AL");
        assert_eq!(derive_initials("Mary Ada Lovelace"), "MAL");
        assert_eq!(
            derive_initials("Mary Ada Beth Lovelace"),
            "MAB",
            "max 3 chars"
        );
        assert_eq!(derive_initials(""), "U", "empty falls back");
        assert_eq!(derive_initials("   "), "U", "whitespace falls back");
    }

    /// `derive_initials` is multi-byte safe: a non-ASCII name like
    /// "Efe Taşkıran" produces "ET" (two ASCII upper-cases) — the
    /// `to_uppercase` API returns an iterator so a single Unicode
    /// scalar may emit multiple chars (e.g. ß → SS). Guard the
    /// truncation against that edge.
    #[test]
    fn derive_initials_handles_unicode_names() {
        assert_eq!(derive_initials("Efe Taşkıran"), "ET");
        // ß uppercases to "SS" — the function would otherwise
        // overflow the 3-char cap if multiple words start with ß.
        assert_eq!(derive_initials("ßeta ßeta ßeta"), "SSS");
    }
}

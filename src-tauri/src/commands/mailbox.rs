//! `mailbox:*` namespace.
//!
//! - `mailbox:list` `(sinceTs?)` → `MailboxEntry[]`
//! - `mailbox:emit` `(entry)` → `MailboxEntry`
//!
//! `mailbox:emit` MUST also fire a `mailbox:new` Tauri event (the
//! Tauri-legal form of ADR-0006's `mailbox.new` — Tauri 2.10 forbids
//! `.` in event names) whose payload equals the inserted
//! `MailboxEntry`. See ADR-0006.
//!
//! ## Stable id derivation
//!
//! The schema stores mailbox rows append-only and uses `(ts, from_pane,
//! to_pane)` as the natural key, but the frontend wants a stable
//! React-list key. SQLite gives us `rowid` per insert which we surface
//! as `id`. Because two emits on the same `ts` would otherwise collide
//! in keys, we rely on the implicit autoincrement ordering of `rowid`.

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::models::{MailboxEntry, MailboxEntryInput};

/// Optional input used by `mailbox:list` to scope to entries newer
/// than a given epoch second. Frontends typically pass the latest
/// `ts` they have cached.
#[derive(Debug, Serialize, Type, Clone, Copy)]
#[allow(dead_code)]
struct SinceTsMarker;

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_list(
    pool: State<'_, DbPool>,
    since_ts: Option<i64>,
) -> Result<Vec<MailboxEntry>, AppError> {
    let rows: Vec<MailboxEntry> = match since_ts {
        Some(t) => {
            sqlx::query_as::<_, MailboxEntry>(
                "SELECT rowid AS id, ts, from_pane, to_pane, type, summary \
                 FROM mailbox WHERE ts >= ? ORDER BY ts DESC, rowid DESC",
            )
            .bind(t)
            .fetch_all(pool.inner())
            .await?
        }
        None => {
            sqlx::query_as::<_, MailboxEntry>(
                "SELECT rowid AS id, ts, from_pane, to_pane, type, summary \
                 FROM mailbox ORDER BY ts DESC, rowid DESC",
            )
            .fetch_all(pool.inner())
            .await?
        }
    };
    Ok(rows)
}

/// Insert one row, return the inserted entry, **and** emit a
/// `mailbox.new` Tauri event with that entry as payload (ADR-0006).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_emit<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    entry: MailboxEntryInput,
) -> Result<MailboxEntry, AppError> {
    if entry.from_pane.trim().is_empty() {
        return Err(AppError::InvalidInput("fromPane must not be empty".into()));
    }
    if entry.to_pane.trim().is_empty() {
        return Err(AppError::InvalidInput("toPane must not be empty".into()));
    }

    let ts = now_seconds();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) \
         VALUES (?, ?, ?, ?, ?) RETURNING rowid",
    )
    .bind(ts)
    .bind(&entry.from_pane)
    .bind(&entry.to_pane)
    .bind(&entry.entry_type)
    .bind(&entry.summary)
    .fetch_one(pool.inner())
    .await?;

    let inserted = MailboxEntry {
        id,
        ts,
        from_pane: entry.from_pane,
        to_pane: entry.to_pane,
        entry_type: entry.entry_type,
        summary: entry.summary,
    };

    // Per ADR-0006: emit a `mailbox.new` event after the insert. The
    // event payload IS the inserted entry — frontends merge into the
    // TanStack Query cache via `qc.setQueryData(['mailbox'], …)`.
    //
    // Tauri 2.10 rejects `.` in event names; the wire form is
    // `mailbox:new`. The logical name in ADR-0006 reads `mailbox.new`,
    // and the WP-W2-08 frontend subscribes via the same colon form.
    app.emit("mailbox:new", &inserted)?;
    Ok(inserted)
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
    //! The `event_fires_after_emit` test is the linchpin acceptance
    //! item: WP-W2-03 § "Acceptance criteria" requires
    //!
    //!   `mailbox:emit` fires a `mailbox:new` Tauri event after a
    //!   successful insert; verified by a unit test that listens
    //!   before invoking and asserts the event payload equals the
    //!   returned `MailboxEntry`.
    //!
    //! Tauri's mock runtime exposes `app.listen("event", handler)`
    //! which works against `AppHandle::emit` calls. We listen, invoke
    //! the command, and parse the captured payload from the event
    //! channel.

    use super::*;
    use crate::test_support::fresh_pool;
    use std::sync::{Arc, Mutex};
    use tauri::{Listener, Manager as _};

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
    async fn mailbox_list_empty_returns_empty_vec() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let state = app.state::<crate::db::DbPool>();
        let out = mailbox_list(state, None).await.expect("ok");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn mailbox_list_filters_by_since_ts() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query(
            "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) VALUES \
             (100,'p1','p2','task:done','old'), \
             (200,'p1','p2','task:done','new')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let state = app.state::<crate::db::DbPool>();

        let recent = mailbox_list(state, Some(150)).await.expect("ok");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].summary, "new");
    }

    #[tokio::test]
    async fn mailbox_emit_inserts_row_and_returns_entry() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let handle = app.handle().clone();
        let state = app.state::<crate::db::DbPool>();

        let inserted = mailbox_emit(
            handle,
            state,
            MailboxEntryInput {
                from_pane: "p1".into(),
                to_pane: "p2".into(),
                entry_type: "task:done".into(),
                summary: "draft patch ready".into(),
            },
        )
        .await
        .expect("ok");
        assert_eq!(inserted.from_pane, "p1");
        assert_eq!(inserted.to_pane, "p2");
        assert_eq!(inserted.entry_type, "task:done");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailbox")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    /// Acceptance: `mailbox:emit` must fire a `mailbox:new` event
    /// (logical name in ADR-0006: `mailbox.new`; Tauri-legal wire form:
    /// `mailbox:new`) whose payload equals the inserted `MailboxEntry`.
    /// We attach a listener before invoking and verify the JSON
    /// payload round-trips back to the same entry.
    #[tokio::test]
    async fn mailbox_emit_fires_mailbox_new_event() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let captured_w = Arc::clone(&captured);
        app.listen("mailbox:new", move |event| {
            *captured_w.lock().unwrap() = Some(event.payload().to_string());
        });

        let handle = app.handle().clone();
        let state = app.state::<crate::db::DbPool>();
        let inserted = mailbox_emit(
            handle,
            state,
            MailboxEntryInput {
                from_pane: "p1".into(),
                to_pane: "p2".into(),
                entry_type: "task:done".into(),
                summary: "hi".into(),
            },
        )
        .await
        .expect("ok");

        // Drive the runtime briefly so the emitted event reaches the
        // listener. The mock runtime processes synchronously but the
        // listener side may queue; yield to let the channel drain.
        tokio::task::yield_now().await;

        let payload = captured
            .lock()
            .unwrap()
            .clone()
            .expect("mailbox:new event was not delivered to listener");
        let parsed: MailboxEntry =
            serde_json::from_str(&payload).expect("parse mailbox.new payload");
        assert_eq!(parsed.id, inserted.id);
        assert_eq!(parsed.ts, inserted.ts);
        assert_eq!(parsed.from_pane, inserted.from_pane);
        assert_eq!(parsed.to_pane, inserted.to_pane);
        assert_eq!(parsed.entry_type, inserted.entry_type);
        assert_eq!(parsed.summary, inserted.summary);
    }

    #[tokio::test]
    async fn mailbox_emit_rejects_empty_from_pane() {
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let handle = app.handle().clone();
        let state = app.state::<crate::db::DbPool>();
        let err = mailbox_emit(
            handle,
            state,
            MailboxEntryInput {
                from_pane: "".into(),
                to_pane: "p2".into(),
                entry_type: "task:done".into(),
                summary: "hi".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }
}

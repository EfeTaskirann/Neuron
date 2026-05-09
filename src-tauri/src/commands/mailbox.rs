//! `mailbox:*` namespace.
//!
//! - `mailbox:list` `(sinceTs?)` → `MailboxEntry[]`
//! - `mailbox:emit` `(entry)` → `MailboxEntry`
//! - `mailbox:emit_typed` `(workspaceId, from, to, summary, parentId?, event)` → `MailboxEnvelope` (W5-01)
//! - `mailbox:list_typed` `(kind?, sinceId?, limit?)` → `MailboxEnvelope[]` (W5-01)
//!
//! `mailbox:emit` MUST also fire a `mailbox:new` Tauri event (the
//! Tauri-legal form of ADR-0006's `mailbox.new` — Tauri 2.10 forbids
//! `.` in event names) whose payload equals the inserted
//! `MailboxEntry`. See ADR-0006.
//!
//! `mailbox:emit_typed` (W5-01) routes through the
//! `crate::swarm::MailboxBus` so it persists + broadcasts to
//! in-process subscribers + fires the legacy `mailbox:new` Tauri
//! event for back-compat.
//!
//! ## Stable id derivation
//!
//! Migration 0002 added an explicit `INTEGER PRIMARY KEY AUTOINCREMENT`
//! column on `mailbox` (see report.md §K7). The `RETURNING rowid` /
//! `SELECT rowid AS id` form below resolves to that PK because SQLite
//! aliases `rowid` to an `INTEGER PRIMARY KEY` column. With
//! `AUTOINCREMENT`, ids are monotonic and never reused after a
//! `DELETE`, so the frontend's React keys stay stable across the
//! mailbox lifecycle.

use std::sync::Arc;

use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Emitter, Runtime, State};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::{MailboxEntry, MailboxEntryInput};
use crate::swarm::{MailboxBus, MailboxEnvelope, MailboxEvent};
use crate::time::now_seconds;

/// Optional input used by `mailbox:list` to scope to entries strictly
/// newer than a given epoch second (exclusive). Frontends typically
/// pass the latest `ts` they have cached so the next `mailbox:list`
/// returns only deltas. The exclusive shape avoids redelivering rows
/// at the boundary `ts` on every poll.
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
                 FROM mailbox WHERE ts > ? ORDER BY ts DESC, rowid DESC",
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

/// W4-07 — internal mailbox emit usable from non-command code
/// (the swarm registry calls this during the help loop). Skips the
/// `State<DbPool>` extractor since the caller has its own
/// `DbPool` reference; preserves the same insert + event-emit
/// shape as `mailbox_emit` so mailbox listeners on either path see
/// identical wire data.
///
/// Convention for swarm entries: `from_pane` / `to_pane` use an
/// `agent:<id>` prefix to disambiguate from terminal-pane entries
/// (`pane:<uuid>`); `entry_type` follows `swarm.<verb>` (e.g.
/// `swarm.help_request`, `swarm.help_direct_answer`,
/// `swarm.help_ask_back`, `swarm.help_escalate`). Future
/// frontend-side filter can branch on the `swarm.` prefix.
pub async fn emit_internal<R: Runtime>(
    app: &AppHandle<R>,
    pool: &DbPool,
    from_pane: &str,
    to_pane: &str,
    entry_type: &str,
    summary: &str,
) -> Result<MailboxEntry, AppError> {
    if from_pane.trim().is_empty() {
        return Err(AppError::InvalidInput("from must not be empty".into()));
    }
    if to_pane.trim().is_empty() {
        return Err(AppError::InvalidInput("to must not be empty".into()));
    }
    let ts = now_seconds();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) \
         VALUES (?, ?, ?, ?, ?) RETURNING rowid",
    )
    .bind(ts)
    .bind(from_pane)
    .bind(to_pane)
    .bind(entry_type)
    .bind(summary)
    .fetch_one(pool)
    .await?;
    let inserted = MailboxEntry {
        id,
        ts,
        from_pane: from_pane.to_string(),
        to_pane: to_pane.to_string(),
        entry_type: entry_type.to_string(),
        summary: summary.to_string(),
    };
    app.emit(events::MAILBOX_NEW, &inserted)?;
    Ok(inserted)
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
        return Err(AppError::InvalidInput("from must not be empty".into()));
    }
    if entry.to_pane.trim().is_empty() {
        return Err(AppError::InvalidInput("to must not be empty".into()));
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
    // The wire-name constant lives in `crate::events` (ADR-0006 §
    // "Wire-format substitution" rationale).
    app.emit(events::MAILBOX_NEW, &inserted)?;
    Ok(inserted)
}

// ---------------------------------------------------------------------
// W5-01 — typed event-bus IPCs
// ---------------------------------------------------------------------

/// W5-01 — typed emit for the mailbox event-bus. Persists +
/// broadcasts (in-process) + fires the legacy `mailbox:new` Tauri
/// event for back-compat. Use this from the W5-02 agent dispatcher,
/// W5-03 Coordinator brain, and W5-05 cancel path.
///
/// `event` is a tagged `MailboxEvent` discriminated by `kind`; the
/// SQL row's `kind` column mirrors the same string for indexed
/// filtering. `summary` is the human-readable line that surfaces
/// in the existing mailbox UI.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_emit_typed<R: Runtime>(
    app: AppHandle<R>,
    bus: State<'_, Arc<MailboxBus>>,
    workspace_id: String,
    from_pane: String,
    to_pane: String,
    summary: String,
    parent_id: Option<i64>,
    event: MailboxEvent,
) -> Result<MailboxEnvelope, AppError> {
    bus.emit_typed(
        &app,
        &workspace_id,
        &from_pane,
        &to_pane,
        &summary,
        parent_id,
        event,
    )
    .await
}

/// W5-01 — typed list with kind filter + since-id cursor. Used by
/// the W5-04 projector to replay events on mount and by the future
/// "Swarm comms" tab UI.
///
/// Returns oldest-first so consumers can replay events in event-log
/// order. `since_id` is exclusive (rows with `id > since_id`).
/// `kind` matches against the SQL `kind` column verbatim — pass
/// e.g. `"task_dispatch"` to get only dispatch events. `limit`
/// defaults to 100, capped at 500.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_list_typed(
    bus: State<'_, Arc<MailboxBus>>,
    kind: Option<String>,
    since_id: Option<i64>,
    limit: Option<u32>,
) -> Result<Vec<MailboxEnvelope>, AppError> {
    bus.list_typed(kind.as_deref(), since_id, limit).await
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
    use crate::test_support::mock_app_with_pool;
    use std::sync::{Arc, Mutex};
    use tauri::{Listener, Manager as _};

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

    /// Boundary: `sinceTs` is exclusive — when it equals an existing
    /// row's `ts`, that row is NOT redelivered. Frontends pass their
    /// cached latest `ts` and expect a strict-greater filter so the
    /// same row isn't pushed on every poll.
    #[tokio::test]
    async fn mailbox_list_since_ts_is_exclusive() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        sqlx::query(
            "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) VALUES \
             (100,'p1','p2','task:done','at-100'), \
             (200,'p1','p2','task:done','at-200')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // sinceTs == 200 must yield an empty list (200 is the latest).
        let none = mailbox_list(app.state::<crate::db::DbPool>(), Some(200))
            .await
            .expect("ok");
        assert!(none.is_empty(), "ts > 200 must be empty, got {none:?}");

        // sinceTs == 100 must yield the 200-row only (the 100-row is on
        // the boundary and excluded by `>`).
        let one = mailbox_list(app.state::<crate::db::DbPool>(), Some(100))
            .await
            .expect("ok");
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].summary, "at-200");
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

    // -----------------------------------------------------------------
    // W5-01 — typed IPC tests
    // -----------------------------------------------------------------

    /// Acceptance: mailbox_emit_typed validates empty inputs the
    /// same way mailbox_emit does (workspace, from, to). Empty
    /// summary is allowed (free-form note shape).
    #[tokio::test]
    async fn mailbox_emit_typed_validates_empty_inputs() {
        let (_app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool));
        let app_with_bus = tauri::test::mock_builder()
            .manage(bus.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let handle = app_with_bus.handle().clone();
        let state = app_with_bus.state::<Arc<MailboxBus>>();

        let err = mailbox_emit_typed(
            handle.clone(),
            state.clone(),
            "".into(),
            "agent:scout".into(),
            "agent:planner".into(),
            "".into(),
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        let err = mailbox_emit_typed(
            handle,
            state,
            "default".into(),
            "".into(),
            "agent:planner".into(),
            "".into(),
            None,
            MailboxEvent::Note,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acceptance: mailbox_emit_typed → mailbox_list_typed round-trip.
    /// Two emits, one filtered list, ordering preserved oldest-first.
    #[tokio::test]
    async fn mailbox_emit_typed_persists_and_lists_typed() {
        let (_app, pool, _dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool));
        let app = tauri::test::mock_builder()
            .manage(bus.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let handle = app.handle().clone();
        let state = app.state::<Arc<MailboxBus>>();

        mailbox_emit_typed(
            handle.clone(),
            state.clone(),
            "default".into(),
            "agent:scout".into(),
            "agent:planner".into(),
            "go".into(),
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:planner".into(),
                prompt: "p".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit dispatch");

        mailbox_emit_typed(
            handle,
            state.clone(),
            "default".into(),
            "agent:planner".into(),
            "agent:scout".into(),
            "ok".into(),
            None,
            MailboxEvent::AgentResult {
                job_id: "j-1".into(),
                agent_id: "planner".into(),
                assistant_text: "done".into(),
                total_cost_usd: 0.0,
                turn_count: 1,
            },
        )
        .await
        .expect("emit result");

        let dispatches =
            mailbox_list_typed(state.clone(), Some("task_dispatch".into()), None, None)
                .await
                .expect("list dispatches");
        assert_eq!(dispatches.len(), 1);
        assert!(matches!(
            dispatches[0].event,
            MailboxEvent::TaskDispatch { .. }
        ));

        let all = mailbox_list_typed(state, None, None, None)
            .await
            .expect("list all");
        assert_eq!(all.len(), 2);
        assert!(matches!(all[0].event, MailboxEvent::TaskDispatch { .. }));
        assert!(matches!(all[1].event, MailboxEvent::AgentResult { .. }));
    }
}

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

// W4-07's `emit_internal` (registry-side mailbox emit for the help
// loop) was deleted: its last caller went with the W5-06 FSM removal
// — swarm-side emits route through `MailboxBus::emit_typed` now.

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
mod tests;

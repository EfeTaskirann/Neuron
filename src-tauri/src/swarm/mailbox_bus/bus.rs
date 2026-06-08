//! `MailboxBus` — the per-workspace pub/sub primitive plus the
//! persist + broadcast + Tauri-emit `emit_typed` / `list_typed` /
//! `cancel_in_flight_brain_jobs` surface.

use std::collections::HashMap;

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::MailboxEntry;
use crate::time::now_seconds;

use super::{MailboxEnvelope, MailboxEvent};

/// Capacity of each per-workspace broadcast channel. `64` is well
/// past the burst rate of any single dispatch (a single Coordinator
/// brain turn produces at most O(10) events: dispatch + result +
/// optional help round-trip + job lifecycle bookends). Receivers
/// that lag past `64` get a `RecvError::Lagged` and skip ahead —
/// acceptable since the SQL log is the source of truth; consumers
/// can recover via `mailbox:list_typed(since_id)` after a lag event.
const BROADCAST_CAPACITY: usize = 64;

/// Default page size for `list_typed`. Mirrors the
/// `SWARM_LIST_JOBS_DEFAULT_LIMIT` shape in `commands/swarm.rs`.
const LIST_TYPED_DEFAULT_LIMIT: u32 = 100;

/// Hard cap to prevent runaway queries.
const LIST_TYPED_MAX_LIMIT: u32 = 500;

/// Per-workspace pubsub for typed mailbox events. Held in
/// `app.manage(...)` next to `SwarmAgentRegistry` (W4-02) and the
/// `DbPool`. Lazy-creates the broadcast channel for a workspace on
/// first `subscribe` / `emit_typed` call.
///
/// Thread-safety: outer `RwLock<HashMap>` guards structural
/// changes (insert new workspace channel); reads dominate (existing
/// workspace lookup on every emit / subscribe), so the read lock
/// keeps the hot path uncontended. Per-workspace `broadcast::Sender`
/// is `Send + Sync` — concurrent emits to the same workspace serialise
/// at the SQL layer (sqlite write lock), not here.
pub struct MailboxBus {
    pool: DbPool,
    channels: RwLock<HashMap<String, broadcast::Sender<MailboxEnvelope>>>,
}

impl MailboxBus {
    /// Construct a fresh bus bound to a SQLite pool. Does NOT
    /// create any broadcast channels — those lazy-create on first
    /// use per workspace.
    pub fn new(pool: DbPool) -> Self {
        Self {
            pool,
            channels: RwLock::new(HashMap::new()),
        }
    }

    /// Get a receiver for the named workspace. Creates the channel
    /// on first call. Subsequent subscribers share the same channel
    /// (so a single emit fans out to all of them).
    pub async fn subscribe(
        &self,
        workspace_id: &str,
    ) -> broadcast::Receiver<MailboxEnvelope> {
        // Fast path: read lock + existing channel.
        {
            let map = self.channels.read().await;
            if let Some(sender) = map.get(workspace_id) {
                return sender.subscribe();
            }
        }
        // Slow path: write lock + insert if still missing (handles
        // the read-then-write race between two concurrent
        // subscribers).
        let mut map = self.channels.write().await;
        let sender = map
            .entry(workspace_id.to_string())
            .or_insert_with(|| broadcast::channel::<MailboxEnvelope>(BROADCAST_CAPACITY).0);
        sender.subscribe()
    }

    /// Persist + broadcast + Tauri-emit one event. Atomic to the
    /// extent SQLite + Tauri allow:
    /// 1. WP-W5-05: if `event` is `JobStarted`, refuse with
    ///    `AppError::WorkspaceBusy` when another brain-driven,
    ///    non-terminal job already exists for the same workspace.
    ///    Other variants skip the guard — only the job-lifecycle
    ///    bookend gates workspace exclusivity.
    /// 2. INSERT row (autoincrement id).
    /// 3. Build envelope from row + event.
    /// 4. `broadcast::Sender::send` to in-process subscribers.
    ///    Send error (no receivers) is silently swallowed — agents
    ///    may not be subscribed yet.
    /// 5. `app.emit("mailbox:new", legacy_form)` for back-compat.
    ///
    /// Any SQL failure short-circuits the rest. Caller-supplied
    /// `from_pane` / `to_pane` use the W4-07 namespacing convention
    /// (`agent:<id>` for swarm rows, `pane:<uuid>` for terminal-pane
    /// rows). The bus does not enforce the format.
    ///
    /// `summary` is the human-readable line; it's persisted alongside
    /// the structured payload so the existing mailbox UI keeps
    /// working unchanged.
    pub async fn emit_typed<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        workspace_id: &str,
        from_pane: &str,
        to_pane: &str,
        summary: &str,
        parent_id: Option<i64>,
        event: MailboxEvent,
    ) -> Result<MailboxEnvelope, AppError> {
        if workspace_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }
        if from_pane.trim().is_empty() {
            return Err(AppError::InvalidInput("from must not be empty".into()));
        }
        if to_pane.trim().is_empty() {
            return Err(AppError::InvalidInput("to must not be empty".into()));
        }

        // WP-W5-05 — workspace-busy guard for brain-driven JobStarted.
        // The W3 FSM enforces "one job per workspace" via the
        // `swarm_workspace_locks` table + `try_acquire_workspace`;
        // for the W5 brain path that lock is short-circuited (the
        // brain runs inline of the IPC, not through the FSM), so the
        // mailbox bus becomes the canonical chokepoint. A non-empty
        // count of brain-driven, non-terminal `swarm_jobs` rows for
        // the target workspace — *excluding the JobStarted's own
        // job_id* — means another brain job is already in-flight;
        // refuse the JobStarted before the row lands so the
        // projector never sees two concurrent JobStarted rows for
        // the same workspace. The current job is excluded because
        // the W5-03 v2 IPC writes its `swarm_jobs` row up-front via
        // `JobRegistry::try_acquire_workspace` *before* emitting
        // JobStarted; without the exclusion the guard would always
        // trip on the just-acquired job. The query targets `source
        // = 'brain'` so the FSM (`source = 'fsm'`) and the brain
        // coexist peacefully until W5-06 deletes the FSM. Other
        // event kinds (TaskDispatch / AgentResult / …) skip the
        // guard — only the job-lifecycle bookend gates workspace
        // exclusivity.
        if let MailboxEvent::JobStarted { job_id: own_job_id, .. } = &event {
            let in_flight: Option<String> = sqlx::query_scalar(
                "SELECT id FROM swarm_jobs \
                 WHERE workspace_id = ? AND source = 'brain' \
                   AND id != ? \
                   AND state NOT IN ('done', 'failed') \
                 ORDER BY created_at_ms DESC LIMIT 1",
            )
            .bind(workspace_id)
            .bind(own_job_id)
            .fetch_optional(&self.pool)
            .await?;
            if let Some(in_flight_job_id) = in_flight {
                return Err(AppError::WorkspaceBusy {
                    workspace_id: workspace_id.to_string(),
                    in_flight_job_id,
                });
            }
        }

        let kind = event.kind_str();
        // Serialize the full tagged-enum JSON. `from_row_parts`
        // round-trips the same shape on the read side.
        let payload_json = serde_json::to_string(&event).map_err(|e| {
            AppError::Internal(format!("mailbox event serialise error: {e}"))
        })?;

        // The legacy `type` column was the primary discriminator
        // for terminal-pane / swarm-help-loop rows. New W5-01
        // emits set `type` = same as `kind` so the legacy column
        // stays informative without forcing callers to supply two
        // strings. Existing mailbox listeners that filtered on
        // `type` will see the new snake_case kind values too —
        // but their existing values (`task:done`, `swarm.help_request`)
        // never collide with snake_case discriminator values, so
        // no false matches.
        let entry_type = kind;

        let ts = now_seconds();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO mailbox \
               (ts, from_pane, to_pane, type, summary, kind, parent_id, payload_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING rowid",
        )
        .bind(ts)
        .bind(from_pane)
        .bind(to_pane)
        .bind(entry_type)
        .bind(summary)
        .bind(kind)
        .bind(parent_id)
        .bind(&payload_json)
        .fetch_one(&self.pool)
        .await?;

        let envelope = MailboxEnvelope {
            id,
            ts,
            from_pane: from_pane.to_string(),
            to_pane: to_pane.to_string(),
            summary: summary.to_string(),
            parent_id,
            event,
        };

        // Broadcast to in-process subscribers. We do NOT lazy-create
        // the channel here — if no subscriber has ever called
        // `subscribe` for this workspace, there's nobody to wake,
        // and creating an empty channel would just leak a Sender.
        {
            let map = self.channels.read().await;
            if let Some(sender) = map.get(workspace_id) {
                // SendError fires when no receivers attached. The
                // SQL log is the source of truth; broadcast is a
                // wake-up optimization — silently swallow.
                let _ = sender.send(envelope.clone());
            }
        }

        // Back-compat: fire the legacy `mailbox:new` Tauri event so
        // existing frontend listeners (terminal pane mailbox panel)
        // keep working. The payload is the legacy `MailboxEntry`
        // shape — same fields the W2-02 emit path produces.
        let legacy_entry = MailboxEntry {
            id,
            ts,
            from_pane: from_pane.to_string(),
            to_pane: to_pane.to_string(),
            entry_type: entry_type.to_string(),
            summary: summary.to_string(),
        };
        app.emit(events::MAILBOX_NEW, &legacy_entry)?;

        Ok(envelope)
    }

    /// Read events with optional `kind` filter and `since_id` cursor.
    /// Returns oldest-first so the projector (W5-04) can replay
    /// events in order. Defaults: `since_id = 0` (all rows),
    /// `limit = 100`, capped at `500`.
    ///
    /// Single-workspace assumption: W5-01 ships without a
    /// `workspace_id` column on the mailbox table. The bus's
    /// per-workspace channel handles in-process fan-out; the SQL
    /// log is single-table single-workspace. A multi-workspace
    /// future (post-W5) adds the column + filter; for W5 the list
    /// returns every row matching the kind/since filter.
    pub async fn list_typed(
        &self,
        kind: Option<&str>,
        since_id: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<MailboxEnvelope>, AppError> {
        let limit = limit
            .unwrap_or(LIST_TYPED_DEFAULT_LIMIT)
            .min(LIST_TYPED_MAX_LIMIT);
        let since_id = since_id.unwrap_or(0);

        // Narrow the SQL to just the columns we need; don't bind
        // optional args dynamically — sqlx doesn't accept `Option`s
        // directly in `bind`. Two query branches keeps SQL static.
        let rows: Vec<(i64, i64, String, String, String, Option<i64>, String, String)> =
            match kind {
                Some(k) => {
                    sqlx::query_as(
                        "SELECT rowid, ts, from_pane, to_pane, summary, \
                                parent_id, kind, payload_json \
                         FROM mailbox \
                         WHERE rowid > ? AND kind = ? \
                         ORDER BY rowid ASC \
                         LIMIT ?",
                    )
                    .bind(since_id)
                    .bind(k)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
                }
                None => {
                    sqlx::query_as(
                        "SELECT rowid, ts, from_pane, to_pane, summary, \
                                parent_id, kind, payload_json \
                         FROM mailbox \
                         WHERE rowid > ? \
                         ORDER BY rowid ASC \
                         LIMIT ?",
                    )
                    .bind(since_id)
                    .bind(limit as i64)
                    .fetch_all(&self.pool)
                    .await?
                }
            };

        rows.into_iter()
            .map(|(id, ts, from_pane, to_pane, summary, parent_id, kind, payload_json)| {
                let event = MailboxEvent::from_row_parts(&kind, &payload_json)?;
                Ok(MailboxEnvelope {
                    id,
                    ts,
                    from_pane,
                    to_pane,
                    summary,
                    parent_id,
                    event,
                })
            })
            .collect()
    }

    /// WP-W5-05 — emit `JobCancel` for every brain-driven,
    /// non-terminal `swarm_jobs` row. Used by the
    /// `RunEvent::ExitRequested` shutdown hook so the brain + each
    /// dispatcher can unwind cleanly before the agent registry tears
    /// the underlying `claude` sessions down.
    ///
    /// Returns the number of cancels that emitted successfully —
    /// failed emits are swallowed (logged via the SQL row's INSERT
    /// failure path) so a single broken row does not block the rest
    /// of the shutdown chain. The test suite uses the return value
    /// to assert exact fan-out counts.
    pub async fn cancel_in_flight_brain_jobs<R: Runtime>(
        &self,
        app: &AppHandle<R>,
    ) -> usize {
        let rows: Vec<(String, String)> = match sqlx::query_as(
            "SELECT id, workspace_id FROM swarm_jobs \
             WHERE source = 'brain' \
               AND state NOT IN ('done', 'failed')",
        )
        .fetch_all(&self.pool)
        .await
        {
            Ok(r) => r,
            Err(_) => return 0,
        };
        let mut emitted = 0_usize;
        for (job_id, ws) in rows {
            if self
                .emit_typed(
                    app,
                    &ws,
                    "agent:user",
                    "agent:coordinator",
                    &format!("shutdown cancel: {job_id}"),
                    None,
                    MailboxEvent::JobCancel { job_id },
                )
                .await
                .is_ok()
            {
                emitted += 1;
            }
        }
        emitted
    }

    /// Test helper: returns the number of workspace channels
    /// currently held in the bus. Used by the
    /// `mailbox_bus_subscribe_creates_channel_on_first_call` test.
    #[cfg(test)]
    pub async fn channel_count(&self) -> usize {
        self.channels.read().await.len()
    }
}

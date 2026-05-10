//! `MailboxBus` — typed event-bus over the W2-02 `mailbox` table
//! (WP-W5-01).
//!
//! Two responsibilities:
//!
//! 1. **Persist** structured events to the SQL log. The
//!    migration 0010 columns (`kind`, `parent_id`, `payload_json`)
//!    extend the existing append-only mailbox so a process restart
//!    can replay the full event history. The `MailboxEvent` enum
//!    captures every shape the W5 swarm needs (task dispatch,
//!    agent result, help requests, job lifecycle).
//!
//! 2. **Broadcast** events in-process so subscribers (W5-02
//!    agent dispatchers, W5-03 Coordinator brain, W5-04
//!    job-state projector) wake on emit instead of polling the
//!    SQL log every 2s. Per-workspace `tokio::sync::broadcast`
//!    channels keep workspaces isolated and cheap to create
//!    on-demand.
//!
//! ## Wire form vs SQL form
//!
//! `MailboxEvent` uses `#[serde(tag = "kind", rename_all =
//! "snake_case")]`. The serde tag value (`task_dispatch`,
//! `agent_result`, …) is the same string written to the SQL
//! `kind` column AND emitted on the JSON IPC bindings — one form,
//! no conversion table to maintain. The legacy `type` column
//! (W2-02 + W4-07: values like `task:done`, `swarm.help_request`)
//! stays untouched on legacy emit paths; W5-01's new emitters
//! keep `summary` for the human-readable line and put the
//! discriminated payload in `kind` + `payload_json`.
//!
//! ## Persistence vs broadcast atomicity
//!
//! `emit_typed` runs in this order:
//! 1. INSERT row (autoincrement id) inside one SQL statement.
//! 2. Build the `MailboxEnvelope` from the row + event.
//! 3. `broadcast::Sender::send` to in-process subscribers.
//!    Errors when no receivers are attached are silently swallowed
//!    (the SQL row IS the source of truth; broadcast is a
//!    wake-up optimization).
//! 4. `app.emit("mailbox:new", legacy_form)` so existing frontend
//!    listeners (terminal pane mailbox panel) keep working.
//!
//! SQL failure short-circuits the rest — the broadcast and Tauri
//! event are skipped so subscribers never see a broadcast that
//! never made it to disk.
//!
//! Out of scope (per WP §"Out of scope"):
//! - Agent-side subscription wiring (W5-02 owns the
//!   `MailboxAgentDispatcher` task).
//! - Coordinator brain dispatch loop (W5-03).
//! - Job-state derivation from mailbox (W5-04).
//! - Cancel propagation through the bus (W5-05).
//! - FSM teardown (W5-06).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::MailboxEntry;
use crate::time::now_seconds;

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

// ---------------------------------------------------------------------
// MailboxEvent — typed payload variants
// ---------------------------------------------------------------------

/// Structured event-bus payload. Discriminated by `kind` field on
/// the wire (snake_case). The variant body carries the typed
/// payload; the SQL row's `payload_json` column persists the same
/// JSON verbatim so a process restart can rebuild events from
/// SQLite without losing fidelity.
///
/// Matches the W5-overview event table:
/// `task_dispatch / agent_result / agent_help_request /
/// coordinator_help_outcome / job_started / job_finished /
/// job_cancel / note`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxEvent {
    /// W5-03: Coordinator brain dispatches a task to a specific
    /// agent. `target` is `agent:<id>` per the W4-07 namespacing.
    /// `prompt` is the user-message fed into the agent's session.
    /// `with_help_loop` toggles the W4-05 help-loop on the dispatch;
    /// defaults to true for builders/scout/planner, false for
    /// reviewers/tester whose persona contracts forbid help blocks.
    TaskDispatch {
        job_id: String,
        target: String,
        prompt: String,
        with_help_loop: bool,
    },
    /// W5-02: agent emitted a result for a dispatch. The
    /// originating `TaskDispatch` row's `id` is carried in the
    /// envelope's `parent_id` (NOT the variant body) so a single
    /// reply-to chain stays uniform across all variants.
    AgentResult {
        job_id: String,
        agent_id: String,
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// W5-02: agent emitted a `neuron_help` block via W4-05's
    /// parser. The W5-03 brain reads this and replies with a
    /// `CoordinatorHelpOutcome`.
    AgentHelpRequest {
        job_id: String,
        agent_id: String,
        reason: String,
        question: String,
    },
    /// W5-03: Coordinator's response to a help request. The
    /// `outcome_json` payload is a JSON-serialised
    /// `swarm::help_request::CoordinatorHelpOutcome` (action +
    /// answer / followup / user_question fields, depending on
    /// variant). Stored as `String` here so the bus stays
    /// decoupled from `swarm::help_request` (which depends on
    /// `swarm::agent_registry` — would create a cycle), and so
    /// the type implements `specta::Type` cleanly. Consumers
    /// parse via `serde_json::from_str`.
    CoordinatorHelpOutcome {
        job_id: String,
        target_agent_id: String,
        outcome_json: String,
    },
    /// W5-03: job lifecycle start. Emitted once per job by the
    /// `swarm:run_job_v2` IPC; CoordinatorBrain subscribes and
    /// drives the dispatch loop.
    JobStarted {
        job_id: String,
        workspace_id: String,
        goal: String,
    },
    /// W5-03: job lifecycle finish. Emitted by CoordinatorBrain
    /// when the brain returns a `finish` action. `outcome` is
    /// `"done" | "failed"`.
    JobFinished {
        job_id: String,
        outcome: String,
        summary: String,
    },
    /// W5-05: cancel signal. CoordinatorBrain + agent dispatchers
    /// subscribe; in-flight turns truncate.
    JobCancel {
        job_id: String,
    },
    /// Legacy free-form note. The default kind for back-compat
    /// emitters (`mailbox::emit_internal` / `mailbox_emit` IPCs
    /// keep emitting `kind='note'` implicitly via the migration
    /// 0010 column default).
    Note,
}

impl MailboxEvent {
    /// SQL `kind` string for this variant. Stable; matches both
    /// the migration's column values AND the JSON wire-form `kind`
    /// tag, so SQL filters and JSON deserialize agree byte-for-byte.
    pub fn kind_str(&self) -> &'static str {
        match self {
            MailboxEvent::TaskDispatch { .. } => "task_dispatch",
            MailboxEvent::AgentResult { .. } => "agent_result",
            MailboxEvent::AgentHelpRequest { .. } => "agent_help_request",
            MailboxEvent::CoordinatorHelpOutcome { .. } => {
                "coordinator_help_outcome"
            }
            MailboxEvent::JobStarted { .. } => "job_started",
            MailboxEvent::JobFinished { .. } => "job_finished",
            MailboxEvent::JobCancel { .. } => "job_cancel",
            MailboxEvent::Note => "note",
        }
    }

    /// Reverse of `kind_str` + JSON parse. Used by the projector
    /// (W5-04) and `list_typed` to rebuild events from SQLite rows.
    ///
    /// `payload_json` MUST carry the full tagged-enum JSON
    /// (including the `kind` field) — `MailboxBus::emit_typed`
    /// always serialises the whole event so this is byte-for-byte
    /// what landed in SQL.
    ///
    /// On parse failure, returns `AppError::Internal` rather than
    /// `AppError::InvalidInput` because a malformed payload here
    /// implies a SQL row written by a buggy emitter, not a user
    /// supplying a bad input.
    pub fn from_row_parts(
        kind: &str,
        payload_json: &str,
    ) -> Result<Self, AppError> {
        // Default-empty body ('{}') with kind='note' decodes as
        // Note via the tagged-enum form `{"kind":"note"}`. The
        // migration 0010 default is `'{}'` not `'{"kind":"note"}'`,
        // so we splice the `kind` tag in if the payload is empty
        // or doesn't already carry it.
        let trimmed = payload_json.trim();
        let parsed: serde_json::Value = if trimmed.is_empty() || trimmed == "{}" {
            serde_json::json!({ "kind": kind })
        } else {
            let mut v: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| AppError::Internal(format!(
                    "mailbox payload_json parse error: {e}"
                )))?;
            // If the payload is a JSON object missing the kind tag,
            // splice it in so the tagged-enum deserialise can pick
            // the variant. Defensive: emitters always write the
            // full tagged form, but legacy 'note' rows from before
            // W5-01 have payload_json='{}' and we want them to
            // round-trip cleanly.
            if let serde_json::Value::Object(map) = &mut v {
                if !map.contains_key("kind") {
                    map.insert(
                        "kind".to_string(),
                        serde_json::Value::String(kind.to_string()),
                    );
                }
            }
            v
        };
        serde_json::from_value(parsed).map_err(|e| {
            AppError::Internal(format!(
                "mailbox event deserialise error (kind={kind}): {e}"
            ))
        })
    }
}

// ---------------------------------------------------------------------
// MailboxEnvelope — wire shape returned by emit_typed / list_typed
// ---------------------------------------------------------------------

/// One persisted row decorated with the typed event. Returned by
/// `MailboxBus::emit_typed` and `MailboxBus::list_typed`. The `id`,
/// `ts`, `from`, `to`, `summary` mirror `MailboxEntry`'s wire shape
/// so the frontend can render either type with the same code path.
///
/// `parent_id` is `None` for top-level events (`JobStarted`,
/// `Note`); `Some(rowid)` for chained events (`AgentResult` whose
/// parent is a `TaskDispatch`).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MailboxEnvelope {
    pub id: i64,
    /// Unix epoch seconds (matches the existing `mailbox.ts`
    /// column; Charter §8 invariant).
    pub ts: i64,
    /// `agent:<id>` for swarm-driven rows, `pane:<uuid>` for
    /// terminal-pane rows. The bus does not enforce a format;
    /// convention is documented in the WP.
    #[serde(rename = "from")]
    pub from_pane: String,
    #[serde(rename = "to")]
    pub to_pane: String,
    /// Human-readable line; mirrors the existing `mailbox.summary`
    /// column. Frontend renders this on the Recent activity list.
    pub summary: String,
    /// Reply-to / correlation reference. Points at another mailbox
    /// row's autoincrement `id`. `None` for top-level events.
    pub parent_id: Option<i64>,
    /// Typed event payload. Tagged on `kind` so the wire form is
    /// `{"kind":"task_dispatch","job_id":"...","target":"...","prompt":"...","with_help_loop":true}`.
    /// Field names stay snake_case inside the variant body
    /// (matches the enum's `rename_all = "snake_case"`); only the
    /// outer envelope renames to camelCase.
    pub event: MailboxEvent,
}

// ---------------------------------------------------------------------
// MailboxBus — pub/sub primitive
// ---------------------------------------------------------------------

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
    /// 1. INSERT row (autoincrement id).
    /// 2. Build envelope from row + event.
    /// 3. `broadcast::Sender::send` to in-process subscribers.
    ///    Send error (no receivers) is silently swallowed — agents
    ///    may not be subscribed yet.
    /// 4. `app.emit("mailbox:new", legacy_form)` for back-compat.
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

    /// Test helper: returns the number of workspace channels
    /// currently held in the bus. Used by the
    /// `mailbox_bus_subscribe_creates_channel_on_first_call` test.
    #[cfg(test)]
    pub async fn channel_count(&self) -> usize {
        self.channels.read().await.len()
    }
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;
    use std::sync::{Arc, Mutex};
    use tauri::Listener;

    // -----------------------------------------------------------------
    // 1. Migration round-trip
    // -----------------------------------------------------------------

    /// Acceptance: migration 0010 lands the three new columns with
    /// defaults; existing-row backfill works; new rows can carry
    /// non-default values.
    #[tokio::test]
    async fn migration_0010_round_trip() {
        let (_, pool, _dir) = mock_app_with_pool().await;

        // Insert a legacy-shape row (no kind / parent_id /
        // payload_json supplied). Defaults must apply.
        sqlx::query(
            "INSERT INTO mailbox (ts, from_pane, to_pane, type, summary) \
             VALUES (100, 'pane:p1', 'pane:p2', 'task:done', 'legacy')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (kind, parent_id, payload_json): (String, Option<i64>, String) =
            sqlx::query_as(
                "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='legacy'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "note");
        assert_eq!(parent_id, None);
        assert_eq!(payload_json, "{}");

        // Insert a new-shape row with non-default values.
        sqlx::query(
            "INSERT INTO mailbox \
               (ts, from_pane, to_pane, type, summary, kind, parent_id, payload_json) \
             VALUES (200, 'agent:scout', 'agent:planner', 'task_dispatch', \
                     'dispatched', 'task_dispatch', 1, '{\"kind\":\"task_dispatch\",\"job_id\":\"j-1\",\"target\":\"agent:scout\",\"prompt\":\"go\",\"with_help_loop\":true}')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let (kind2, parent2, payload2): (String, Option<i64>, String) =
            sqlx::query_as(
                "SELECT kind, parent_id, payload_json FROM mailbox WHERE summary='dispatched'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind2, "task_dispatch");
        assert_eq!(parent2, Some(1));
        assert!(payload2.contains("\"job_id\":\"j-1\""));
    }

    // -----------------------------------------------------------------
    // 2. MailboxEvent round-trip
    // -----------------------------------------------------------------

    fn sample_events() -> Vec<MailboxEvent> {
        vec![
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:scout".into(),
                prompt: "Investigate auth.rs".into(),
                with_help_loop: true,
            },
            MailboxEvent::AgentResult {
                job_id: "j-1".into(),
                agent_id: "scout".into(),
                assistant_text: "Found three matches.".into(),
                total_cost_usd: 0.012_5,
                turn_count: 3,
            },
            MailboxEvent::AgentHelpRequest {
                job_id: "j-1".into(),
                agent_id: "backend-builder".into(),
                reason: "Plan step ambiguous".into(),
                question: "Which struct field carries the user id?".into(),
            },
            MailboxEvent::CoordinatorHelpOutcome {
                job_id: "j-1".into(),
                target_agent_id: "backend-builder".into(),
                outcome_json: r#"{"action":"direct_answer","answer":"User.id"}"#.into(),
            },
            MailboxEvent::JobStarted {
                job_id: "j-1".into(),
                workspace_id: "default".into(),
                goal: "Refactor auth".into(),
            },
            MailboxEvent::JobFinished {
                job_id: "j-1".into(),
                outcome: "done".into(),
                summary: "All approved.".into(),
            },
            MailboxEvent::JobCancel {
                job_id: "j-1".into(),
            },
            MailboxEvent::Note,
        ]
    }

    #[test]
    fn mailbox_event_kind_str_round_trip() {
        for event in sample_events() {
            let kind = event.kind_str();
            let payload_json = serde_json::to_string(&event).unwrap();
            let restored =
                MailboxEvent::from_row_parts(kind, &payload_json).unwrap();
            assert_eq!(restored, event, "round-trip drift on {kind}");
        }
    }

    #[test]
    fn mailbox_event_from_row_parts_handles_each_variant() {
        // Eight variants — one fixture each. The kind_str_round_trip
        // test already covers serde-emitted JSON; this one
        // additionally checks hand-written JSON shapes that the
        // frontend might emit through the IPC.
        let cases: &[(&str, &str, fn(&MailboxEvent) -> bool)] = &[
            (
                "task_dispatch",
                r#"{"kind":"task_dispatch","job_id":"j-1","target":"agent:scout","prompt":"go","with_help_loop":false}"#,
                |e| matches!(e, MailboxEvent::TaskDispatch { with_help_loop: false, .. }),
            ),
            (
                "agent_result",
                r#"{"kind":"agent_result","job_id":"j-1","agent_id":"scout","assistant_text":"done","total_cost_usd":0.5,"turn_count":2}"#,
                |e| matches!(e, MailboxEvent::AgentResult { turn_count: 2, .. }),
            ),
            (
                "agent_help_request",
                r#"{"kind":"agent_help_request","job_id":"j-1","agent_id":"x","reason":"r","question":"q"}"#,
                |e| matches!(e, MailboxEvent::AgentHelpRequest { .. }),
            ),
            (
                "coordinator_help_outcome",
                r#"{"kind":"coordinator_help_outcome","job_id":"j-1","target_agent_id":"x","outcome_json":"{\"action\":\"direct_answer\",\"answer\":\"a\"}"}"#,
                |e| matches!(e, MailboxEvent::CoordinatorHelpOutcome { .. }),
            ),
            (
                "job_started",
                r#"{"kind":"job_started","job_id":"j-1","workspace_id":"default","goal":"g"}"#,
                |e| matches!(e, MailboxEvent::JobStarted { .. }),
            ),
            (
                "job_finished",
                r#"{"kind":"job_finished","job_id":"j-1","outcome":"done","summary":"s"}"#,
                |e| matches!(e, MailboxEvent::JobFinished { .. }),
            ),
            (
                "job_cancel",
                r#"{"kind":"job_cancel","job_id":"j-1"}"#,
                |e| matches!(e, MailboxEvent::JobCancel { .. }),
            ),
            ("note", r#"{}"#, |e| matches!(e, MailboxEvent::Note)),
        ];
        for (kind, payload, predicate) in cases {
            let event = MailboxEvent::from_row_parts(kind, payload)
                .expect(&format!("parse {kind}"));
            assert!(predicate(&event), "predicate failed for kind={kind}");
        }
    }

    #[test]
    fn mailbox_event_from_row_parts_rejects_malformed_payload() {
        // 1) non-JSON garbage
        let err = MailboxEvent::from_row_parts(
            "task_dispatch",
            "not even close to json",
        )
        .unwrap_err();
        assert_eq!(err.kind(), "internal");

        // 2) JSON object missing required fields
        let err = MailboxEvent::from_row_parts(
            "task_dispatch",
            r#"{"kind":"task_dispatch","target":"x"}"#,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "internal");

        // 3) JSON array (wrong shape entirely)
        let err = MailboxEvent::from_row_parts(
            "task_dispatch",
            r#"["task_dispatch"]"#,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "internal");

        // 4) Unknown variant kind
        let err = MailboxEvent::from_row_parts(
            "totally_made_up",
            r#"{"kind":"totally_made_up"}"#,
        )
        .unwrap_err();
        assert_eq!(err.kind(), "internal");
    }

    #[test]
    fn mailbox_event_from_row_parts_handles_empty_payload_with_kind() {
        // Legacy 'note' row with payload_json='{}' (the migration
        // 0010 default) should round-trip to MailboxEvent::Note via
        // the kind splice.
        let event =
            MailboxEvent::from_row_parts("note", "{}").unwrap();
        assert_eq!(event, MailboxEvent::Note);

        // Whitespace also handled.
        let event =
            MailboxEvent::from_row_parts("note", "  ").unwrap();
        assert_eq!(event, MailboxEvent::Note);
    }

    // -----------------------------------------------------------------
    // 3. MailboxBus subscribe / channel lifecycle
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn mailbox_bus_subscribe_creates_channel_on_first_call() {
        let (_, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);
        assert_eq!(bus.channel_count().await, 0);
        let _rx = bus.subscribe("default").await;
        assert_eq!(bus.channel_count().await, 1);
        // Different workspace — separate channel.
        let _rx2 = bus.subscribe("other").await;
        assert_eq!(bus.channel_count().await, 2);
    }

    #[tokio::test]
    async fn mailbox_bus_subscribe_shares_channel_across_calls() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);
        let mut rx1 = bus.subscribe("default").await;
        let mut rx2 = bus.subscribe("default").await;
        assert_eq!(bus.channel_count().await, 1);

        // One emit; both subscribers receive.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "kicked off",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-1".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .expect("emit");

        let env1 = rx1.recv().await.expect("rx1 recv");
        let env2 = rx2.recv().await.expect("rx2 recv");
        assert_eq!(env1.id, env2.id);
        assert!(matches!(env1.event, MailboxEvent::JobStarted { .. }));
    }

    // -----------------------------------------------------------------
    // 4. emit_typed end-to-end
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn mailbox_bus_emit_persists_row() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool.clone());
        let env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:planner",
                "summary text",
                Some(42),
                MailboxEvent::TaskDispatch {
                    job_id: "j-1".into(),
                    target: "agent:planner".into(),
                    prompt: "do the thing".into(),
                    with_help_loop: true,
                },
            )
            .await
            .expect("emit");

        let (kind, parent, payload, summary): (
            String,
            Option<i64>,
            String,
            String,
        ) = sqlx::query_as(
            "SELECT kind, parent_id, payload_json, summary FROM mailbox WHERE rowid=?",
        )
        .bind(env.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(kind, "task_dispatch");
        assert_eq!(parent, Some(42));
        assert_eq!(summary, "summary text");
        assert!(payload.contains("\"target\":\"agent:planner\""));
    }

    #[tokio::test]
    async fn mailbox_bus_emit_broadcasts_envelope() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);
        let mut rx = bus.subscribe("default").await;

        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "broadcasted",
            None,
            MailboxEvent::JobCancel {
                job_id: "j-1".into(),
            },
        )
        .await
        .expect("emit");

        let env = rx.recv().await.expect("rx recv");
        assert_eq!(env.from_pane, "agent:scout");
        assert!(matches!(env.event, MailboxEvent::JobCancel { .. }));
    }

    #[tokio::test]
    async fn mailbox_bus_emit_swallows_broadcast_send_error_on_no_subscribers() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);
        // No subscribers — emit must succeed.
        let result = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:planner",
                "no listeners",
                None,
                MailboxEvent::Note,
            )
            .await;
        assert!(result.is_ok(), "emit failed without subscribers: {result:?}");
    }

    #[tokio::test]
    async fn mailbox_bus_emit_fires_legacy_mailbox_new_event() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let captured: Arc<Mutex<Option<String>>> =
            Arc::new(Mutex::new(None));
        let captured_w = Arc::clone(&captured);
        app.listen("mailbox:new", move |event| {
            *captured_w.lock().unwrap() = Some(event.payload().to_string());
        });

        let bus = MailboxBus::new(pool);
        let env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:planner",
                "back-compat",
                None,
                MailboxEvent::JobStarted {
                    job_id: "j-1".into(),
                    workspace_id: "default".into(),
                    goal: "g".into(),
                },
            )
            .await
            .expect("emit");

        // Drive runtime briefly so the listener picks up the event.
        tokio::task::yield_now().await;

        let payload = captured
            .lock()
            .unwrap()
            .clone()
            .expect("legacy mailbox:new event was not delivered");
        let parsed: MailboxEntry = serde_json::from_str(&payload)
            .expect("parse legacy MailboxEntry");
        assert_eq!(parsed.id, env.id);
        assert_eq!(parsed.from_pane, "agent:scout");
        assert_eq!(parsed.entry_type, "job_started");
    }

    #[tokio::test]
    async fn mailbox_bus_emit_validates_inputs() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);

        let err = bus
            .emit_typed(
                app.handle(),
                "",
                "agent:scout",
                "agent:planner",
                "",
                None,
                MailboxEvent::Note,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        let err = bus
            .emit_typed(
                app.handle(),
                "default",
                "",
                "agent:planner",
                "",
                None,
                MailboxEvent::Note,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");

        let err = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "",
                "",
                None,
                MailboxEvent::Note,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "invalid_input");
    }

    // -----------------------------------------------------------------
    // 5. list_typed
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn mailbox_list_typed_filters_by_kind() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);

        // Mix of kinds.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:planner",
            "started",
            None,
            MailboxEvent::JobStarted {
                job_id: "j-1".into(),
                workspace_id: "default".into(),
                goal: "g".into(),
            },
        )
        .await
        .unwrap();
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:builder",
            "dispatched",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:builder".into(),
                prompt: "build".into(),
                with_help_loop: true,
            },
        )
        .await
        .unwrap();
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:builder",
            "agent:planner",
            "result",
            None,
            MailboxEvent::AgentResult {
                job_id: "j-1".into(),
                agent_id: "builder".into(),
                assistant_text: "done".into(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .unwrap();

        let dispatches =
            bus.list_typed(Some("task_dispatch"), None, None).await.unwrap();
        assert_eq!(dispatches.len(), 1);
        assert!(matches!(dispatches[0].event, MailboxEvent::TaskDispatch { .. }));

        let all = bus.list_typed(None, None, None).await.unwrap();
        assert_eq!(all.len(), 3);
        // Oldest-first ordering: job_started < task_dispatch < agent_result.
        assert!(matches!(all[0].event, MailboxEvent::JobStarted { .. }));
        assert!(matches!(all[2].event, MailboxEvent::AgentResult { .. }));
    }

    #[tokio::test]
    async fn mailbox_list_typed_paginates_by_since_id() {
        let (app, pool, _dir) = mock_app_with_pool().await;
        let bus = MailboxBus::new(pool);

        let env1 = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:a",
                "agent:b",
                "1",
                None,
                MailboxEvent::Note,
            )
            .await
            .unwrap();
        let _env2 = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:a",
                "agent:b",
                "2",
                None,
                MailboxEvent::Note,
            )
            .await
            .unwrap();

        let after_first =
            bus.list_typed(None, Some(env1.id), None).await.unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].summary, "2");
    }
}

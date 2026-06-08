//! `MailboxEnvelope` — the wire shape returned by
//! `MailboxBus::emit_typed` / `MailboxBus::list_typed`.

use serde::{Deserialize, Serialize};
use specta::Type;

use super::event::MailboxEvent;

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

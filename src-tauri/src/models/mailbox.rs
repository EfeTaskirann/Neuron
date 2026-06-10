//! Mailbox domain types (`mailbox` table + emit input).
//!
//! Wire keys `from`/`to` match the terminal-data mock per Charter
//! Constraint #1; Rust fields keep the `_pane` suffix for SQL column
//! binding and code clarity (see the module-level § "Mailbox wire keys"
//! note in [`crate::models`]).

use serde::{Deserialize, Serialize};
use specta::Type;

/// One row of `mailbox`. Wire keys `from`/`to` match the terminal-data
/// mock per Charter Constraint #1; Rust fields keep the `_pane` suffix
/// for SQL column binding and code clarity (see § "Mailbox wire keys"
/// at the top of this module).
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct MailboxEntry {
    /// Stable autoincrement id from migration 0002
    /// (`INTEGER PRIMARY KEY AUTOINCREMENT`). Per ADR-0007 §3, mailbox
    /// is the canonical autoincrement-int domain — opaque to
    /// consumers, used solely as a React key. Monotonic, never reused
    /// after `DELETE`.
    pub id: i64,
    /// Unix epoch seconds.
    pub ts: i64,
    #[serde(rename = "from")]
    pub from_pane: String,
    #[serde(rename = "to")]
    pub to_pane: String,
    /// Cross-pane event type, e.g. `task:done`.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub entry_type: String,
    pub summary: String,
}

/// Input shape for `mailbox:emit`. `ts` is filled server-side at
/// insert time; the frontend just describes the message. Wire keys
/// `from`/`to` per Charter Constraint #1.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MailboxEntryInput {
    #[serde(rename = "from")]
    pub from_pane: String,
    #[serde(rename = "to")]
    pub to_pane: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub summary: String,
}

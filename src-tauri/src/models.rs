//! Domain types exchanged across the Tauri IPC boundary.
//!
//! Per WP-W2-03 § "Notes / risks":
//!
//!   Field names must match the frontend mock shapes from
//!   `Neuron Design/app/data.js` (camelCase via serde rename or
//!   naturally compatible).
//!
//! All structs derive:
//!
//! - `Serialize`/`Deserialize` so they cross the IPC boundary.
//! - `specta::Type` so `bindings.ts` carries them as TypeScript types.
//! - `sqlx::FromRow` where they map directly to a table column set,
//!   to keep query handlers terse.
//! - `Debug`/`Clone` for unit-test ergonomics.
//!
//! ## Frontend-shape parity
//!
//! Where the SQL column name and the mock key disagree we use
//! `#[serde(rename = "...")]` to emit the mock's key. Examples:
//!
//! - `Server.description` → `desc` (the mock writes `desc:`).
//! - `Run.duration_ms`    → `dur`  (mock writes `dur:`).
//! - `Run.cost_usd`       → `cost` (mock writes `cost:`).
//! - `Run.workflow_name`  → `workflow` (mock writes `workflow:`).
//!
//! ## Mailbox wire keys
//!
//! `MailboxEntry` wire shape uses `from`/`to` to match the terminal-data
//! mock per Charter Constraint #1. Rust struct fields keep the `_pane`
//! suffix (`from_pane`/`to_pane`) so they bind cleanly to the SQL
//! columns of the same name and read unambiguously in code that handles
//! cross-pane events; `#[serde(rename = "from"|"to")]` does the
//! wire-side translation. Pre-2026-04-29 versions of this module
//! shipped `fromPane`/`toPane` on the wire as a "canonical contract"
//! deviation; that deviation was reversed when Charter Constraint #1
//! was reaffirmed against display-derived carve-outs only.

use serde::{Deserialize, Serialize};
use specta::Type;

// ---------------------------------------------------------------------
// Agents
// ---------------------------------------------------------------------

/// One row of `agents`. Mirrors `data.js#agents[]` exactly.
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub model: String,
    pub temp: f64,
    pub role: String,
}

/// Input shape for `agents:create`. `id` is generated server-side
/// (ULID), so the frontend supplies only the user-visible fields.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentCreateInput {
    pub name: String,
    pub model: String,
    pub temp: f64,
    pub role: String,
}

/// Input shape for `agents:update`. Every field is optional — only the
/// fields actually sent are written. `id` is the URL parameter, not a
/// patch field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentPatch {
    pub name: Option<String>,
    pub model: Option<String>,
    pub temp: Option<f64>,
    pub role: Option<String>,
}

// ---------------------------------------------------------------------
// Workflows
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Workflow {
    pub id: String,
    pub name: String,
    /// Unix epoch seconds when the workflow was last saved. Mirrors
    /// the WP-W2-02 column.
    pub saved_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub workflow_id: String,
    /// Constrained at SQL level to `'llm'|'tool'|'logic'|'human'|'mcp'`.
    pub kind: String,
    pub x: i64,
    pub y: i64,
    pub title: String,
    pub meta: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub id: String,
    pub workflow_id: String,
    pub from_node: String,
    pub to_node: String,
    /// SQLite stores 0/1 INTEGER; sqlx decodes that to `bool`
    /// natively (`sqlx_sqlite::types::bool`).
    pub active: bool,
}

/// Composite payload for `workflows:get` — the workflow itself plus
/// its full node and edge list. Matches the WP signature
/// `→ { workflow: Workflow; nodes: Node[]; edges: Edge[] }`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDetail {
    pub workflow: Workflow,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

// ---------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------

/// One row of `runs`. Field renames map the storage column names to
/// the keys the frontend mock uses (`workflow`, `dur`, `cost`).
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    /// Denormalized workflow display name. Mock key: `workflow`.
    #[serde(rename = "workflow")]
    pub workflow_name: String,
    /// Original workflow id (FK target). Carried so the inspector
    /// can deep-link without an extra `runs:get` round-trip.
    pub workflow_id: String,
    /// Unix epoch seconds. Mock key in `data.js` is the formatted
    /// relative-time string `started`; the wire format here is a
    /// number to stay type-stable, and the frontend hook formats it
    /// for display.
    pub started_at: i64,
    /// Mock key: `dur` (milliseconds). `Option` because a still-
    /// running run has no duration yet.
    #[serde(rename = "dur")]
    pub duration_ms: Option<i64>,
    /// Mock key: `tokens`.
    pub tokens: i64,
    /// Mock key: `cost`.
    #[serde(rename = "cost")]
    pub cost_usd: f64,
    /// `'running'|'success'|'error'` (CHECK-constrained at SQL).
    pub status: String,
}

/// Optional filter for `runs:list`. Currently scopes by status only;
/// extended by WP-04 once real run execution lands.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RunFilter {
    pub status: Option<String>,
    pub workflow_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub id: String,
    pub run_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    /// `'llm'|'tool'|'logic'|'human'|'http'`.
    #[sqlx(rename = "type")]
    #[serde(rename = "type")]
    pub span_type: String,
    pub t0_ms: i64,
    pub duration_ms: Option<i64>,
    pub attrs_json: String,
    pub prompt: Option<String>,
    pub response: Option<String>,
    pub is_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RunDetail {
    pub run: Run,
    pub spans: Vec<Span>,
}

// ---------------------------------------------------------------------
// MCP servers
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub by: String,
    /// Mock key: `desc`.
    #[serde(rename = "desc")]
    pub description: String,
    pub installs: i64,
    pub rating: f64,
    pub featured: bool,
    pub installed: bool,
}

/// One row of `server_tools`. Materialised by [`crate::mcp::registry`]
/// during `mcp:install`; consumed by the agent runtime (WP-W2-04) and
/// surfaced to the frontend via `mcp:listTools`.
///
/// `input_schema_json` is stored as a TEXT column (raw JSON Schema)
/// so the frontend can hand it directly to a JSON-Schema validator
/// without re-encoding. The wire shape uses `inputSchemaJson` to make
/// the schema-vs-string distinction explicit.
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub server_id: String,
    pub name: String,
    pub description: String,
    pub input_schema_json: String,
}

/// One block of a `tools/call` response. Mirrors the MCP spec's
/// content array element. We expose `text` natively and pass any
/// other shape through as `other` so the UI can render unknown blocks
/// best-effort instead of failing the whole call.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Other,
}

/// Wire shape for `mcp:callTool` returns. Keeps a flat `{content,
/// isError}` object so the frontend can rely on a single deserializer
/// regardless of which tool was called.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    pub is_error: bool,
}

// ---------------------------------------------------------------------
// Terminal panes
// ---------------------------------------------------------------------

/// One row of `panes`. Maps `agent_kind` to `agent` to match the
/// terminal-data mock's `agent` key.
///
/// The trailing five fields (`tokens_in`/`tokens_out`/`cost_usd`/
/// `uptime`/`approval`) exist for mock-shape parity per Charter
/// Constraint #1; their wire values are sourced as follows:
///
/// - `tokens_in`/`tokens_out`/`cost_usd` — Week 2 always ship `None`;
///   Week 3 will aggregate them from `runs_spans`.
/// - `uptime` — backend always ships `None` per Charter #1
///   *display-derived carve-out* (the frontend hook computes the
///   `"12m 04s"` string from `started_at`).
/// - `approval` — populated by `commands::terminal::terminal_list`
///   from `panes.last_approval_json` *only* when `status =
///   'awaiting_approval'`. The reader writes the JSON blob in
///   `sidecar::terminal` on every regex match; the column is
///   intentionally NOT cleared when the pane re-enters `running`,
///   so a future debug view can replay the last seen banner.
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    pub id: String,
    pub workspace: String,
    /// Mock key: `agent`. The DB column is `agent_kind` because
    /// `agent` is a reserved word in some SQL dialects and the
    /// schema future-proofs the rename. The wire stays `agent`.
    #[serde(rename = "agent")]
    #[sqlx(rename = "agent_kind")]
    pub agent_kind: String,
    pub role: Option<String>,
    pub cwd: String,
    pub status: String,
    pub pid: Option<i64>,
    pub started_at: i64,
    pub closed_at: Option<i64>,
    /// Aggregate input tokens consumed by this pane's agent runtime.
    /// Week 2: always `None` — populated in Week 3 from `runs_spans`.
    #[sqlx(default)]
    pub tokens_in: Option<i64>,
    /// Aggregate output tokens. Week 2: always `None`.
    #[sqlx(default)]
    pub tokens_out: Option<i64>,
    /// Aggregate USD cost. Week 2: always `None`.
    #[sqlx(default)]
    pub cost_usd: Option<f64>,
    /// Display-derived `"12m 04s"` string. Per Charter Constraint #1
    /// carve-out: backend ships `None`; the frontend hook computes
    /// from `started_at`. Field exists for mock-shape parity only.
    #[sqlx(default)]
    pub uptime: Option<String>,
    /// Approval banner blob extracted from the most recent
    /// `awaiting_approval` regex match. `None` when the pane is not
    /// currently awaiting approval; the underlying DB column may
    /// still hold a stale blob from a previous awaiting cycle so the
    /// reader path is resilient to noisy retries.
    ///
    /// `#[sqlx(skip)]`: `ApprovalBanner` is JSON-on-disk and does not
    /// implement `sqlx::Decode`. `terminal_list` reads the raw
    /// `last_approval_json` TEXT column via a manual row mapping and
    /// hydrates this field after-the-fact; everywhere else (e.g.
    /// `terminal_spawn` `RETURNING`) defaults to `None`.
    #[sqlx(skip)]
    pub approval: Option<ApprovalBanner>,
}

/// One approval banner blob. Populated by the terminal reader when
/// an `awaiting_approval` regex matches; surfaced to the UI's amber
/// banner strip per `NEURON_TERMINAL_REPORT.md`. Mock parity:
/// `terminal-data.js#panes[0].approval` —
/// `{tool, target, added, removed}`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalBanner {
    pub tool: String,
    pub target: String,
    pub added: i64,
    pub removed: i64,
}

/// Input shape for `terminal:spawn`. Fields chosen to match what the
/// WP-06 PTY supervisor needs; `cwd` is the only strictly-required
/// field — `agentKind` is inferred from `cmd` when omitted, and
/// `cmd`/`cols`/`rows` fall back to the platform default shell at
/// `80x24`.
///
/// WP-W2-06 added `cmd`/`cols`/`rows`. The legacy WP-03 callers
/// supplied `agentKind` directly; the field is now optional and
/// defaults to either the substring match against `cmd` (claude-code /
/// codex / gemini) or `"shell"` when no inference is possible.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PaneSpawnInput {
    pub cwd: String,
    /// Override the default platform shell. `None` = pick automatically
    /// (`pwsh.exe`/`powershell.exe` on Windows, `$SHELL` elsewhere).
    pub cmd: Option<String>,
    /// Initial PTY column count. `None` = 80.
    pub cols: Option<u16>,
    /// Initial PTY row count. `None` = 24.
    pub rows: Option<u16>,
    /// Optional explicit agent kind. `None` triggers substring inference
    /// from `cmd` (`claude-code`/`codex`/`gemini`/`shell`).
    pub agent_kind: Option<String>,
    pub role: Option<String>,
    pub workspace: Option<String>,
}

/// One line of pane scrollback as returned by `terminal:lines`. Mirrors
/// the per-line entries in the in-memory ring buffer and the `pane_lines`
/// table column set; `seq` is the monotonic per-pane sequence number,
/// `k` matches the schema's CHECK list (`'sys'|'prompt'|'command'|...`),
/// and `text` is the LF-terminated line text with CSI cursor-control
/// stripped (raw bytes still flow through the live Tauri event for
/// xterm.js rendering in WP-W2-08).
#[derive(Debug, Clone, Serialize, Deserialize, Type, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PaneLine {
    pub seq: i64,
    pub k: String,
    pub text: String,
}

// ---------------------------------------------------------------------
// Mailbox
// ---------------------------------------------------------------------

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

// ---------------------------------------------------------------------
// Me (workspace + user composite)
// ---------------------------------------------------------------------

/// User profile fields surfaced in the Sidebar avatar / settings.
/// Mock parity: `Neuron Design/app/data.js#user`.
/// Week 2 hardcoded; Week 3 sources from a settings table.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub initials: String,
    pub name: String,
}

/// Active workspace metadata. `count` is the number of workflows
/// currently saved (denormalised from `SELECT COUNT(*) FROM workflows`).
/// Mock parity: `Neuron Design/app/data.js#workspace`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub name: String,
    pub count: i64,
}

/// Composite shape returned by `me:get`. Combines `data.user` and
/// `data.workspace` so the Sidebar mounts in one round-trip.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user: User,
    pub workspace: Workspace,
}

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
//!
//! ## Module layout
//!
//! The types are grouped by domain into private submodules and
//! re-exported flat below, so every consumer keeps using
//! `crate::models::<Type>` exactly as before:
//!
//! - [`agent`]    — `Agent`, `AgentCreateInput`, `AgentPatch`
//! - [`workflow`] — `Workflow`, `Node`, `Edge`, `WorkflowDetail`
//! - [`run`]      — `Run`, `RunFilter`, `Span`, `RunDetail`
//! - [`mcp`]      — `Server`, `Tool`, `ToolContent`, `CallToolResult`
//! - [`pane`]     — `Pane`, `ApprovalBanner`, `PaneSpawnInput`, `PaneLine`
//! - [`mailbox`]  — `MailboxEntry`, `MailboxEntryInput`
//! - [`me`]       — `User`, `Workspace`, `Me`
//! - [`profile`]  — `ProfileSummary`

mod agent;
mod mailbox;
mod mcp;
mod me;
mod pane;
mod profile;
mod run;
mod workflow;

pub use agent::{Agent, AgentCreateInput, AgentPatch};
pub use mailbox::{MailboxEntry, MailboxEntryInput};
pub use mcp::{CallToolResult, Server, Tool, ToolContent};
pub use me::{Me, User, Workspace};
pub use pane::{ApprovalBanner, Pane, PaneLine, PaneSpawnInput};
pub use profile::ProfileSummary;
pub use run::{Run, RunDetail, RunFilter, Span};
pub use workflow::{Edge, Node, Workflow, WorkflowDetail};

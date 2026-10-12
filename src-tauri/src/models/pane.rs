//! Terminal pane domain types (`panes`/`pane_lines` + spawn input +
//! approval banner).

use serde::{Deserialize, Serialize};
use specta::Type;

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
    /// Extra environment variables to set on the spawned process
    /// AFTER the agent-kind-specific scrub list runs. Used by
    /// `swarm-term` to point each claude pane at a private
    /// `HOME`/`USERPROFILE` directory so the 9 concurrent claude.exe
    /// REPLs don't race on a shared `~/.claude.json` and corrupt it.
    /// `None` = no extra env (typical for terminal:spawn callers).
    pub extra_env: Option<std::collections::HashMap<String, String>>,
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

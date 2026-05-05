//! Canonical Tauri event names emitted by Neuron's backend.
//!
//! See ADR-0006 §"Wire-format substitution" for the `.` → `:` rule
//! and ADR-0006 §"Decision" for the `{domain}{sep}{id?}{sep}{verb}`
//! shape these constants implement. The substitution is forced by
//! Tauri 2.10's event-name validator, which rejects `.` and panics
//! with `IllegalEventName`; the logical shape from the ADR is
//! preserved.
//!
//! New events MUST land here rather than as ad-hoc string literals
//! at the emit site, so a future separator change (or rename) is a
//! single-file edit, not a six-file grep-and-replace.

/// `agents:changed` — coalesced create/update/delete on the agents
/// table. Payload: `{ id, op }`.
pub const AGENTS_CHANGED: &str = "agents:changed";

/// `mailbox:new` — new mailbox row inserted by `mailbox:emit`.
/// Payload: the freshly-inserted `MailboxEntry`.
pub const MAILBOX_NEW: &str = "mailbox:new";

/// `mcp:installed` — MCP server transitioned from uninstalled to
/// installed (after a successful `tools/list`). Payload: `Server`.
pub const MCP_INSTALLED: &str = "mcp:installed";

/// `mcp:uninstalled` — MCP server torn down. Payload: `{ id }`.
pub const MCP_UNINSTALLED: &str = "mcp:uninstalled";

/// `runs:{id}:span` — per-run lifecycle event covering span
/// `created`/`updated`/`closed` (the discriminant lives in the
/// payload's `kind` field). Frontend subscribes per-run.
#[inline]
pub fn run_span(run_id: &str) -> String {
    format!("runs:{run_id}:span")
}

/// `panes:{id}:line` — one line of PTY output for the given pane.
/// Frontend subscribes per-pane.
#[inline]
pub fn pane_line(pane_id: &str) -> String {
    format!("panes:{pane_id}:line")
}

/// `swarm:job:{id}:event` — per-job streaming lifecycle event for
/// the swarm coordinator FSM (WP-W3-12c). Payload is a tagged
/// `SwarmJobEvent` enum with a `kind` discriminator covering
/// `started` / `stage_started` / `stage_completed` / `finished` /
/// `cancelled`. Frontend subscribes per-job; one event name covers
/// every transition so a single listener captures the full stream.
#[inline]
pub fn swarm_job_event(job_id: &str) -> String {
    format!("swarm:job:{job_id}:event")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_event_names_use_colon_separator() {
        assert_eq!(AGENTS_CHANGED, "agents:changed");
        assert_eq!(MAILBOX_NEW, "mailbox:new");
        assert_eq!(MCP_INSTALLED, "mcp:installed");
        assert_eq!(MCP_UNINSTALLED, "mcp:uninstalled");
    }

    #[test]
    fn parametric_event_names_interpolate_id() {
        assert_eq!(run_span("r-01ABC"), "runs:r-01ABC:span");
        assert_eq!(pane_line("p-01XYZ"), "panes:p-01XYZ:line");
        assert_eq!(swarm_job_event("j-01ABC"), "swarm:job:j-01ABC:event");
    }

    /// ADR-0006: Tauri 2.10 rejects `.` in event names. The
    /// substitution rule must hold for every name we emit, including
    /// the parametric ones — otherwise a setup-time panic could
    /// surface only on the first emit instead of at compile time.
    #[test]
    fn no_event_name_contains_dot_separator() {
        for s in [
            AGENTS_CHANGED,
            MAILBOX_NEW,
            MCP_INSTALLED,
            MCP_UNINSTALLED,
        ] {
            assert!(!s.contains('.'), "static event name `{s}` must not contain `.`");
        }
        let dyn_run = run_span("r-1");
        let dyn_pane = pane_line("p-1");
        let dyn_swarm = swarm_job_event("j-1");
        assert!(!dyn_run.contains('.'));
        assert!(!dyn_pane.contains('.'));
        assert!(!dyn_swarm.contains('.'));
    }
}

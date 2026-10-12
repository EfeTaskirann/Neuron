//! `BrainAction` discriminated-union + the defense-in-depth parser
//! and the `max_dispatches` env resolver.
//!
//! Split out of the monolithic `brain.rs` (WP-W5-03). The shapes
//! and parsing strategy are unchanged — only the module boundary
//! moved. The parser mirrors the W3-12d Verdict / W3-12f Decision /
//! W4-05 HelpRequest 4-step strategy.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;

use crate::error::AppError;
use crate::swarm::llm_json::{first_balanced_object, strip_fence};

/// Default cap on `Dispatch` actions per job. Past this many
/// dispatches the brain bails with `JobFinished {outcome:"failed",
/// summary:"exceeded max dispatches"}`. 30 is generous: the
/// FSM's worst-case ExecutePlan + 2 retries chain reaches ~9 stages
/// (Scout / Classify / Plan / Build / Review / Test, plus 2× retry
/// rounds of Plan-Build-Review-Test), and the brain has additional
/// degrees of freedom (parallel build dispatches, reviewer rounds)
/// so 30 leaves headroom without making a runaway loop unbounded.
pub const DEFAULT_MAX_DISPATCHES: u32 = 30;

/// Env override for [`DEFAULT_MAX_DISPATCHES`]. Same reading rules
/// as the existing `NEURON_SWARM_AGENT_TURN_CAP` knob: numeric > 0
/// wins; non-numeric / zero falls back to default with a warn log.
pub(super) const MAX_DISPATCHES_ENV: &str = "NEURON_BRAIN_MAX_DISPATCHES";

/// Maximum bytes of `assistant_text` scanned for a brain-action
/// JSON block. Defends against an adversarial reply mostly composed
/// of garbage with a tiny JSON block hidden in the middle. 16 KiB
/// matches the W4-05 `HELP_REQUEST_SCAN_CAP`.
const BRAIN_ACTION_SCAN_CAP: usize = 16 * 1024;

// ---------------------------------------------------------------------
// BrainAction — discriminated-union of every action the persona may emit
// ---------------------------------------------------------------------

/// One Coordinator-emitted action. Tagged on `action`; field names
/// stay snake_case (matching the W5-01 `MailboxEvent` precedent).
///
/// `body_json` is `String`-typed (not `serde_json::Value`) because
/// `Value` does not implement `specta::Type`. The string carries
/// the serialised JSON payload of a `CoordinatorHelpOutcome`; the
/// W5-02 dispatcher (with_help_loop branch) parses it back via
/// `serde_json::from_str` before feeding to the specialist.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrainAction {
    /// Route a sub-task to a specialist. `target` is `agent:<id>`
    /// per the W5-01 namespacing convention (NOT `<id>` alone —
    /// the dispatcher's `parse_agent_target` strips the prefix).
    /// `with_help_loop` defaults to `false` — opt-in per dispatch.
    Dispatch {
        target: String,
        prompt: String,
        #[serde(default)]
        with_help_loop: bool,
    },
    /// Terminate the job. `outcome` is `"done" | "failed"`; any
    /// other string is normalised to `"failed"` by the brain
    /// before emitting `JobFinished` (matching the W3-12d
    /// "outcome must be one of {done, failed}" hygiene rule).
    Finish {
        outcome: String,
        summary: String,
    },
    /// Surface a question to the user. The orchestrator chat panel
    /// (W5-04+) listens for `JobFinished { outcome: "ask_user" }`
    /// and renders the question; for W5-03 the brain emits
    /// `JobFinished` with `summary` carrying the question text.
    AskUser {
        question: String,
    },
    /// Resolve a specialist's `AgentHelpRequest`. `target` is
    /// `agent:<id>` of the specialist being answered; `body_json`
    /// is a serialised
    /// `swarm::help_request::CoordinatorHelpOutcome`. The brain
    /// emits `MailboxEvent::CoordinatorHelpOutcome` and continues
    /// the dispatch loop — `HelpOutcome` does NOT count toward
    /// the `max_dispatches` cap.
    HelpOutcome {
        target: String,
        body_json: String,
    },
}

// ---------------------------------------------------------------------
// parse_brain_action — defense-in-depth 4-step parser
// ---------------------------------------------------------------------

/// Parse a [`BrainAction`] from the Coordinator's `assistant_text`.
/// Returns `Err(AppError::SwarmInvoke)` when no valid action JSON
/// is present — unlike `parse_help_request` which returns `None`,
/// the brain MUST decide on every turn so missing JSON is a hard
/// error.
///
/// 4-step strategy (matches W3-12d Verdict / W3-12f Decision /
/// W4-05 HelpRequest):
///   1. Whole-text JSON
///   2. ```json (or ```) fence strip
///   3. First balanced `{...}` substring
///   4. Bail with structured error
pub fn parse_brain_action(
    assistant_text: &str,
) -> Result<BrainAction, AppError> {
    // char-boundary-safe: the brain runs inline in the IPC future, so a
    // mid-char slice panic would skip finalise_run_job and wedge the
    // workspace lock for the rest of the session.
    let truncated =
        crate::text::truncate_to_char_boundary(assistant_text, BRAIN_ACTION_SCAN_CAP);

    // 1. Whole-text JSON.
    if let Some(action) = try_parse_brain_action(truncated.trim()) {
        return Ok(action);
    }
    // 2. ```json fence strip.
    if let Some(fenced) = strip_fence(truncated) {
        if let Some(action) = try_parse_brain_action(fenced.trim()) {
            return Ok(action);
        }
    }
    // 3. First balanced {...}.
    if let Some(balanced) = first_balanced_object(truncated) {
        if let Some(action) = try_parse_brain_action(balanced) {
            return Ok(action);
        }
    }
    // 4. Bail.
    Err(AppError::SwarmInvoke(format!(
        "brain action JSON not found in coordinator reply (first 200 chars: {})",
        truncated.chars().take(200).collect::<String>()
    )))
}

/// Helper: inner parse of a candidate JSON fragment as a
/// `BrainAction`. Returns `None` on parse failure so the caller
/// can fall through to the next strategy.
fn try_parse_brain_action(s: &str) -> Option<BrainAction> {
    // Pre-validate the JSON shape so we can distinguish "valid JSON
    // but unknown discriminator" (caller's bug — we surface a
    // SwarmInvoke for it on the bail path) from "non-JSON garbage"
    // (parser fall-through).
    let v: Value = serde_json::from_str(s).ok()?;
    serde_json::from_value::<BrainAction>(v).ok()
}

// Fence-strip + balanced-object scan live in `swarm::llm_json` —
// the shared home of the 4-step extraction recipe.

/// Resolve the per-process max-dispatches cap. Same env-reading
/// pattern as `commands/swarm.rs::stage_timeout`: numeric > 0 wins;
/// non-numeric / zero falls back to default with a warn log.
pub fn resolve_max_dispatches() -> u32 {
    match std::env::var(MAX_DISPATCHES_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>()
        {
            Ok(0) => {
                tracing::warn!(
                    %MAX_DISPATCHES_ENV,
                    "value `0` is not a valid max-dispatches cap; \
                     falling back to default"
                );
                DEFAULT_MAX_DISPATCHES
            }
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    %MAX_DISPATCHES_ENV,
                    raw = %raw,
                    error = %e,
                    "max-dispatches override is not a non-negative \
                     integer; using default"
                );
                DEFAULT_MAX_DISPATCHES
            }
        },
        _ => DEFAULT_MAX_DISPATCHES,
    }
}

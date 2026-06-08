//! Dispatcher tunables: per-invoke timeout + help-loop bounds.

use std::time::Duration;

/// Default per-invoke timeout. Mirrors
/// `commands::swarm::stage_timeout()` (60s default; env override
/// `NEURON_SWARM_STAGE_TIMEOUT_SEC`). Re-implemented here rather
/// than re-exported from the commands module to avoid a swarm →
/// commands cycle.
const DEFAULT_DISPATCH_TIMEOUT_SECS: u64 = 60;
const STAGE_TIMEOUT_ENV: &str = "NEURON_SWARM_STAGE_TIMEOUT_SEC";

/// Cap on help-loop rounds when `with_help_loop: true`. Same cap
/// the deleted `RegistryTransport` used in W4-06; the dispatcher
/// is now the single owner of the help-loop. Past the cap the
/// dispatcher gives up and emits the most recent `assistant_text`
/// (still containing the unanswered `neuron_help` block) as the
/// AgentResult — the brain can decide whether to retry, escalate,
/// or finish:failed.
pub(super) const MAX_HELP_ROUNDS: u32 = 3;

/// Soft timeout for awaiting a `CoordinatorHelpOutcome` after
/// emitting an `AgentHelpRequest`. The brain may take O(seconds)
/// to render its help-decision turn; 120s is generous. Past the
/// timeout the dispatcher emits the prior assistant_text as the
/// AgentResult so the projector/UI never sees an indefinite hang.
pub(super) const HELP_OUTCOME_TIMEOUT_SECS: u64 = 120;

pub(super) fn dispatch_timeout() -> Duration {
    match std::env::var(STAGE_TIMEOUT_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u64>()
        {
            Ok(0) => {
                tracing::warn!(
                    %STAGE_TIMEOUT_ENV,
                    "value `0` is not a valid stage timeout; \
                     falling back to default in dispatcher"
                );
                Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS)
            }
            Ok(secs) => Duration::from_secs(secs),
            Err(e) => {
                tracing::warn!(
                    %STAGE_TIMEOUT_ENV,
                    raw = %raw,
                    error = %e,
                    "stage timeout override is not a non-negative \
                     integer; using default in dispatcher"
                );
                Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS)
            }
        },
        _ => Duration::from_secs(DEFAULT_DISPATCH_TIMEOUT_SECS),
    }
}

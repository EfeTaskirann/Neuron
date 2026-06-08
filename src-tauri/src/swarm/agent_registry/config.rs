//! Per-process turn-cap resolution (`NEURON_SWARM_AGENT_TURN_CAP`).

/// Default hard cap on `turns_taken` before a session is gracefully
/// respawned. Tunable per-process via `NEURON_SWARM_AGENT_TURN_CAP`.
/// 200 is generous — most jobs walk through 5-7 stages, so the
/// average specialist fires < 10 turns per job. Cap at 200 means a
/// session has to absorb ≥ 20 jobs before respawn — well past the
/// "context bloat" point in practice.
pub(super) const DEFAULT_TURN_CAP: u32 = 200;

/// Env override for `DEFAULT_TURN_CAP`. Same reading rules as the
/// stage-timeout pattern in `commands/swarm.rs`: numeric > 0 wins;
/// non-numeric / zero falls back to the default with a warn log.
pub(super) const TURN_CAP_ENV: &str = "NEURON_SWARM_AGENT_TURN_CAP";

/// Resolve the per-process turn cap. Same env-reading shape as
/// `commands/swarm.rs::stage_timeout` so the project has one
/// pattern for tunable env knobs.
pub(super) fn resolve_turn_cap() -> u32 {
    match std::env::var(TURN_CAP_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>() {
            Ok(0) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    "value `0` is not a valid turn cap; falling back to default"
                );
                DEFAULT_TURN_CAP
            }
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    raw = %raw,
                    error = %e,
                    "turn cap override is not a non-negative integer; using default"
                );
                DEFAULT_TURN_CAP
            }
        },
        _ => DEFAULT_TURN_CAP,
    }
}

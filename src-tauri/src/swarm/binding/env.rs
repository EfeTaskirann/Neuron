//! Subscription-only env for a spawned `claude` subprocess. See the
//! module-level docs (`super`) for the responsibility split; this file
//! owns responsibility #2 (auth-routing env strip).

use std::collections::HashMap;

/// Env var names stripped from the spawned process so the `claude`
/// CLI cannot fall back to API-key auth, a non-Anthropic provider, or
/// a parent-supplied OAuth token that overrides the user's
/// `~/.claude/.credentials.json`. Documented at
/// `report/Neuron Multi-Agent Orchestration ...` §3.4.
///
/// Re-exported as `pub(crate)` so the brain spawn paths
/// (`transport::SubprocessTransport`, `swarm::persistent_session`) can
/// iterate over it instead of hard-coding their own redundant
/// `env_remove` calls. The 2026-05-13 `/login` regression pinned the
/// missing entries — `CLAUDE_CODE_OAUTH_TOKEN` in particular leaks
/// from a parent `claude` shell (Neuron launched from within Claude
/// Code) and silently overrides the per-pane credentials seed.
pub(crate) const STRIPPED_ENV_VARS: &[&str] = &[
    // Auth-bypass tokens.
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    // Endpoint overrides — point claude at a different API server,
    // which has its own auth state distinct from the user's session.
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_URL",
    "ANTHROPIC_CUSTOM_HEADERS",
    // Provider-routing toggles (USE_*: legacy; CLAUDE_CODE_USE_*:
    // current). Setting any of these silently flips the spawn off
    // the user's Pro/Max OAuth onto BYOK / cloud billing.
    "USE_BEDROCK",
    "USE_VERTEX",
    "USE_FOUNDRY",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
    "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    // Config-dir override — if set in parent, the child ignores HOME
    // and reads from the override path, defeating the per-process
    // isolation downstream callers may have arranged.
    "CLAUDE_CONFIG_DIR",
];

/// Build the env map for a `claude` spawn. Inherits the parent's env
/// minus the four auth-routing variables in `STRIPPED_ENV_VARS` so the
/// child must use whatever subscription session `~/.claude/.credentials`
/// already holds.
pub fn subscription_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for var in STRIPPED_ENV_VARS {
        env.remove(*var);
    }
    env
}

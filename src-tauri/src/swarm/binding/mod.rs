//! `claude` CLI invocation helpers (WP-W3-11 §3).
//!
//! Three responsibilities:
//!
//! 1. **Resolution** of the host's `claude` binary path. Mirrors
//!    `crate::sidecar::agent::resolve_python`'s 3-step pattern:
//!    explicit env override → `which` PATH lookup → platform-specific
//!    fallback locations.
//! 2. **Subscription-only env** for the spawned subprocess. The
//!    Phase 1 transport must run on the user's Pro / Max OAuth
//!    channel; an injected `ANTHROPIC_API_KEY` would silently flip
//!    `claude` into BYOK billing. Strip it (and the three provider-
//!    routing toggles) so the subprocess inherits everything else
//!    verbatim.
//! 3. **argv builder** for a one-shot per-invoke specialist call. The
//!    flag order is the contract from WP §3 — do not deviate.
//!
//! ## Module layout
//!
//! The three responsibilities above map 1:1 onto submodules; the flat
//! `binding::*` path is preserved by the re-exports below so consumers
//! (`transport::SubprocessTransport`, `swarm::persistent_session`,
//! `commands::swarm_term`, `swarm_term::session`) are untouched:
//!
//! - [`resolve`] — binary/spawn resolution: `ClaudeBinary`,
//!   `ClaudeSpawn`, `resolve_claude_spawn`, `resolve_claude_binary`,
//!   `CLAUDE_BIN_ENV` (+ private `platform_fallback_paths`/`home_dir`).
//! - [`env`] — subscription-only env: `STRIPPED_ENV_VARS`,
//!   `subscription_env`.
//! - [`args`] — argv builder: `build_specialist_args`.

mod args;
mod env;
mod resolve;

#[cfg(test)]
mod tests;

pub use args::build_specialist_args;
pub use env::subscription_env;
pub use resolve::{
    resolve_claude_binary, resolve_claude_spawn, ClaudeBinary, ClaudeSpawn,
    CLAUDE_BIN_ENV,
};

// `pub(crate)` so the brain spawn paths (`transport::SubprocessTransport`,
// `swarm::persistent_session`) can iterate over it via the flat
// `crate::swarm::binding::STRIPPED_ENV_VARS` path.
pub(crate) use env::STRIPPED_ENV_VARS;

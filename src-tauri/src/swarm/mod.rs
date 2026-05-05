//! WP-W3-11 — swarm runtime substrate.
//!
//! The swarm runtime is Neuron's local-only multi-agent orchestration
//! layer: the user picks a team, talks to a Coordinator (W3-12+), and
//! the Coordinator dispatches per-invoke `claude` CLI specialists. It
//! coexists with — but never imports from — the LangGraph Python
//! sidecar at `crate::sidecar::agent`, which continues to power the
//! scripted "Daily summary" demo workflow.
//!
//! The architectural ground truth for this module is the report at
//! `report/Neuron Multi-Agent Orchestration — Mimari Analiz Raporu`,
//! particularly §3 (subprocess pattern) and §13 (smoke validations).
//! The supervisor patterns (`Command` / `BufReader` / `kill_on_drop`)
//! mirror `crate::sidecar::agent`'s long-running supervisor, inverted
//! to one supervisor per call.
//!
//! Phase 1 (this WP) ships substrate only:
//!
//! - `profile`: `.md` frontmatter loader (workspace overrides bundled).
//! - `binding`: `claude` binary resolution + subscription-only env +
//!   per-invoke argv builder.
//! - `transport`: one-shot `claude` subprocess that drives a single
//!   user message through stream-json and returns the `result` event.
//!
//! Higher-layer concerns (Coordinator state machine, persistent chat,
//! retry loop, broadcast / fan-out, multi-pane UI, MCP per-agent
//! config, profile permission-mode enforcement) belong to W3-12+.

pub mod binding;
pub mod coordinator;
pub mod profile;
pub mod transport;

// Re-export the public surface used by `commands/swarm.rs` so callers
// can `use crate::swarm::{ProfileRegistry, ...};` without three
// separate `use` lines per file.
pub use binding::{
    build_specialist_args, resolve_claude_binary, subscription_env,
    ClaudeBinary,
};
pub use coordinator::{
    CoordinatorFsm, Job, JobOutcome, JobRegistry, JobState, StageResult,
    SwarmJobEvent, MAX_RETRIES,
};
pub use profile::{
    PermissionMode, Profile, ProfileRegistry, ProfileSource,
};
pub use transport::{InvokeResult, SubprocessTransport, Transport};

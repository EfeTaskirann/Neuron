//! WP-W3-12a — Coordinator FSM skeleton (DELETED in WP-W5-06).
//! WP-W3-12b — SQLite persistence + restart recovery.
//! WP-W5-06 — FSM module deleted. The brain dispatcher (W5-03) is
//!            now the only orchestration path. This module retains
//!            the still-used parsers (decision, verdict, orchestrator
//!            JSON contracts) plus the W3-12b `JobRegistry`
//!            in-memory state + workspace-lock + cancel-notify
//!            registration that the brain-driven `swarm:run_job`
//!            still leans on for backwards compatibility.
//!
//! After W5-06 the surface here is parsers + persistence helpers +
//! orchestrator chat-thread storage. The state machine that owned
//! the chain is gone; the brain decides dispatch order LLM-side.
//!
//! Cross-runtime hygiene: this module never imports from
//! `crate::sidecar::agent` (the LangGraph Python sidecar) or
//! `crate::agent_runtime`. The two runtimes coexist but stay
//! independent.

pub mod decision;
pub mod job;
pub mod orchestrator;
pub mod orchestrator_session;
pub(crate) mod store;
pub mod verdict;

pub use decision::{parse_decision, CoordinatorDecision, CoordinatorRoute};
pub use job::{
    Job, JobDetail, JobOutcome, JobRegistry, JobState, JobSummary,
    StageResult, SwarmJobEvent,
};
pub use orchestrator::{
    parse_orchestrator_outcome, OrchestratorAction, OrchestratorOutcome,
};
pub use orchestrator_session::{
    OrchestratorMessage, OrchestratorMessageRole,
};
pub use verdict::{parse_verdict, Verdict, VerdictIssue, VerdictSeverity};

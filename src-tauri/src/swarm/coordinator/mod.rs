//! WP-W3-12a — Coordinator FSM skeleton.
//! WP-W3-12b — SQLite persistence + restart recovery.
//!
//! Layer above `crate::swarm::{binding,profile,transport}` (W3-11
//! substrate) that turns the per-invoke `claude` subprocess into a
//! 3-stage chained workflow exposed through a single Tauri IPC
//! (`swarm:run_job`). Walks `scout` → `planner` → `backend-builder` in
//! a fixed order, blocks until the chain terminates (Done / Failed),
//! and serializes per-workspace via the `JobRegistry`.
//!
//! W3-12b layered SQLite write-through onto the same surface — the
//! registry now optionally persists every state transition so jobs
//! survive an app restart (orphan rows flip to Failed on the next
//! `recover_orphans` sweep). W3-12d adds the Verdict gate +
//! reviewer/integration-tester profiles + retry feedback loop.
//! W3-12f wires the single-shot Coordinator brain (Option B routing)
//! between Scout and Plan: a 6th bundled profile (`coordinator.md`)
//! emits a JSON `CoordinatorDecision` that picks `ResearchOnly`
//! (skip the rest of the chain — Scout's findings are the
//! deliverable) or `ExecutePlan` (continue Plan/Build/Review/Test).
//!
//! Cross-runtime hygiene: this module never imports from
//! `crate::sidecar::agent` (the LangGraph Python sidecar) or
//! `crate::agent_runtime`. The two runtimes coexist but stay
//! independent; sharing process state across them is a Coordinator
//! brain concern (W3-13+).

pub mod decision;
pub mod fsm;
pub mod job;
pub mod orchestrator;
pub(crate) mod store;
pub mod verdict;

pub use decision::{parse_decision, CoordinatorDecision, CoordinatorRoute};
pub use fsm::{CoordinatorFsm, MAX_RETRIES};
pub use job::{
    Job, JobDetail, JobOutcome, JobRegistry, JobState, JobSummary,
    StageResult, SwarmJobEvent,
};
pub use orchestrator::{
    parse_orchestrator_outcome, OrchestratorAction, OrchestratorOutcome,
};
pub use verdict::{parse_verdict, Verdict, VerdictIssue, VerdictSeverity};

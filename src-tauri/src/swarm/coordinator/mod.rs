//! WP-W3-12a — Coordinator FSM skeleton.
//!
//! Layer above `crate::swarm::{binding,profile,transport}` (W3-11
//! substrate) that turns the per-invoke `claude` subprocess into a
//! 3-stage chained workflow exposed through a single Tauri IPC
//! (`swarm:run_job`). Walks `scout` → `planner` → `backend-builder` in
//! a fixed order, blocks until the chain terminates (Done / Failed),
//! and serializes per-workspace via the in-memory `JobRegistry`.
//!
//! This is the *skeleton* — pure Rust state machine with no
//! Coordinator LLM brain (Option A in the architectural report
//! §11.4). W3-12b layers SQLite persistence on the same surface,
//! W3-12c adds streaming Tauri events, and W3-12d adds the Verdict
//! gate + reviewer/integration-tester profiles + retry feedback loop.
//!
//! Cross-runtime hygiene: this module never imports from
//! `crate::sidecar::agent` (the LangGraph Python sidecar) or
//! `crate::agent_runtime`. The two runtimes coexist but stay
//! independent; sharing process state across them is a Coordinator
//! brain concern (W3-13+).

pub mod fsm;
pub mod job;

pub use fsm::{CoordinatorFsm, MAX_RETRIES};
pub use job::{Job, JobOutcome, JobRegistry, JobState, StageResult};

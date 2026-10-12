//! Unit tests for the `coordinator::job` package.
//!
//! **Refactor (T3-30, DEEP):** these used to live in a single
//! ~529-line `tests.rs`. The suite is now split to mirror the
//! production submodules, so each test file sits next to the concern
//! it exercises:
//!
//! - [`state`] — `JobState` serde + DB-string round-trips.
//! - [`registry`] — `JobRegistry` acquire/release/cancel + lock map.
//! - [`model`] — `Job::last_rejecting_gate` derivation.
//! - [`event`] — `SwarmJobEvent` wire-shape.
//!
//! `fixture_job` is shared by the `registry` and `model` suites, so it
//! stays here as a `pub(super)` helper; `stage_with_verdict` is used
//! only by `model` and lives there.

use super::{Job, JobState};

mod event;
mod model;
mod registry;
mod state;

pub(super) fn fixture_job(id: &str) -> Job {
    Job {
        id: id.to_string(),
        goal: "test goal".into(),
        created_at_ms: 0,
        state: JobState::Init,
        retry_count: 0,
        stages: Vec::new(),
        last_error: None,
        last_verdict: None,
        source: Job::default_source(),
    }
}

//! Brain test package.
//!
//! Split from the monolithic `brain/tests.rs` (T3-25) along the same
//! seam the production module already uses, mirroring the
//! `commands/swarm/tests/` precedent (T3-24):
//!
//! - [`parser`] — `BrainAction` deserialisation + the 4-step
//!   `parse_brain_action` extractor + `resolve_max_dispatches`
//!   (pure `#[test]`s against [`super::action`]).
//! - [`run_loop`] — the `CoordinatorBrain` dispatch loop exercised
//!   through the `CoordinatorInvoker` seam by a scripted mock
//!   (`#[tokio::test]`s, owning the `setup_brain` /
//!   `wait_for_envelope_log` harness).
//!
//! Each submodule carries its own explicit imports rather than a
//! shared prelude — the parser tests and the run-loop tests have
//! disjoint dependency sets, so no cross-area helper module is
//! warranted.

mod parser;
mod run_loop;

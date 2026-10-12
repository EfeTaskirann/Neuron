//! `JobState` enum tests — serde + DB-string round-trips and the
//! `is_terminal` contract.

use super::super::JobState;

/// Every `JobState` variant serde-roundtrips through the wire
/// shape (specta's camelCase emission) without information loss.
#[test]
fn job_state_transitions_serialize_round_trip() {
    for state in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
        JobState::Done,
        JobState::Failed,
    ] {
        let json =
            serde_json::to_string(&state).expect("serialize");
        let back: JobState =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back, "round-trip failed for {state:?}");
    }
    // Spot-check the on-wire shape so future renames don't
    // silently break the frontend bindings.
    assert_eq!(
        serde_json::to_string(&JobState::Init).unwrap(),
        "\"init\""
    );
    assert_eq!(
        serde_json::to_string(&JobState::Failed).unwrap(),
        "\"failed\""
    );
}

/// `as_db_str` and `from_db_str` round-trip every variant.
/// W3-12b §6 acceptance criterion: "JobState::{as,from}_db_str
/// round-trip on every variant".
#[test]
fn job_state_db_str_round_trips() {
    for state in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
        JobState::Done,
        JobState::Failed,
    ] {
        let s = state.as_db_str();
        let back = JobState::from_db_str(s)
            .unwrap_or_else(|_| panic!("round-trip {state:?} via {s}"));
        assert_eq!(state, back, "db_str round-trip failed for {state:?}");
    }
}

/// Unknown DB-string values surface as `Internal`, not silently
/// mapped to a default state.
#[test]
fn job_state_from_db_str_unknown_errors() {
    let err = JobState::from_db_str("nonsense")
        .expect_err("unknown discriminant rejected");
    assert_eq!(err.kind(), "internal");
}

/// `JobState::is_terminal` matches the documented contract.
#[test]
fn job_state_is_terminal_matches_done_or_failed() {
    assert!(JobState::Done.is_terminal());
    assert!(JobState::Failed.is_terminal());
    for s in [
        JobState::Init,
        JobState::Scout,
        JobState::Classify,
        JobState::Plan,
        JobState::Build,
        JobState::Review,
        JobState::Test,
    ] {
        assert!(!s.is_terminal(), "{s:?} should not be terminal");
    }
}

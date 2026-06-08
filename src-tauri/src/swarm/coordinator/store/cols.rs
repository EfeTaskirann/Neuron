//! JSON column codecs and goal truncation for the store layer.
//!
//! These are leaf helpers shared by the write and read sides. The
//! `Verdict` / `CoordinatorDecision` codecs round-trip `None` to a
//! NULL column and surface parse/serialize failures as a typed
//! `AppError::Internal` so a corrupted DB never silently drops a
//! value. `truncate_chars` is char-bounded (not byte-bounded) so
//! multi-byte Turkish text is never split mid-codepoint.

use crate::error::AppError;
use crate::swarm::coordinator::decision::CoordinatorDecision;
use crate::swarm::coordinator::verdict::Verdict;

/// Serialize an optional `Verdict` to JSON for column storage.
/// `None` round-trips to `Ok(None)` (the column stays NULL).
/// Serialization failure surfaces as `AppError::Internal` —
/// `Verdict` is a closed serde shape so the only realistic failure
/// path is OOM, but we surface a typed error rather than panicking.
pub(super) fn serialize_verdict(
    verdict: Option<&Verdict>,
) -> Result<Option<String>, AppError> {
    match verdict {
        None => Ok(None),
        Some(v) => serde_json::to_string(v)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to serialize Verdict: {e}"
            ))),
    }
}

/// Deserialize an optional JSON column back to a `Verdict`.
/// `None` (NULL column) round-trips to `Ok(None)`; a non-null
/// column that fails to parse surfaces as `AppError::Internal`
/// with the parse error attached so a corrupted DB never silently
/// drops a Verdict.
pub(super) fn deserialize_verdict(
    raw: Option<&str>,
) -> Result<Option<Verdict>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str::<Verdict>(s)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to deserialize Verdict from DB: {e}"
            ))),
    }
}

/// Serialize an optional `CoordinatorDecision` to JSON for column
/// storage (W3-12f). `None` round-trips to `Ok(None)` (the column
/// stays NULL). Mirrors `serialize_verdict` in shape.
pub(super) fn serialize_decision(
    decision: Option<&CoordinatorDecision>,
) -> Result<Option<String>, AppError> {
    match decision {
        None => Ok(None),
        Some(d) => serde_json::to_string(d)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to serialize CoordinatorDecision: {e}"
            ))),
    }
}

/// Deserialize an optional JSON column back to a `CoordinatorDecision`
/// (W3-12f). `None` (NULL column) round-trips to `Ok(None)`; a
/// non-null column that fails to parse surfaces as
/// `AppError::Internal` so a corrupted DB never silently drops a
/// decision.
pub(super) fn deserialize_decision(
    raw: Option<&str>,
) -> Result<Option<CoordinatorDecision>, AppError> {
    match raw {
        None => Ok(None),
        Some(s) => serde_json::from_str::<CoordinatorDecision>(s)
            .map(Some)
            .map_err(|e| AppError::Internal(format!(
                "swarm: failed to deserialize CoordinatorDecision from DB: {e}"
            ))),
    }
}

/// Truncate `s` to at most `max_chars` Unicode characters. Bounded
/// by `chars()` (not bytes) so multi-byte Turkish text is never
/// split mid-codepoint. Returns the original string when it's
/// already within the cap.
pub(super) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

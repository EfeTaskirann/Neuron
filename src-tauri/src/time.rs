//! Time helpers shared across the command surface.
//!
//! Per Charter §"Hard constraints" #8: field names ending in `_at`
//! carry UNIX epoch **seconds**; field names ending in `_ms` carry
//! UNIX epoch **milliseconds**. The helpers here never invent a
//! third unit — callers pick `now_seconds` for `_at` columns and
//! `now_millis` for `_ms` columns and never mix.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UNIX time in **seconds**. Returns 0 on the (impossible
/// in practice) clock-before-epoch case so callers never have to
/// branch on a `Result` for a timestamp that the OS guarantees.
#[inline]
pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Current UNIX time in **milliseconds**. Same fallback as
/// [`now_seconds`].
#[inline]
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the seconds and millis helpers agree to within a
    /// reasonable wall-clock skew. We allow 2s of slop so the test
    /// is not flaky on a slow CI runner.
    #[test]
    fn now_seconds_and_now_millis_agree() {
        let s = now_seconds();
        let ms = now_millis();
        let derived_s = ms / 1000;
        assert!(
            (s - derived_s).abs() <= 2,
            "now_seconds={s} but now_millis/1000={derived_s}"
        );
    }

    #[test]
    fn now_seconds_is_after_2026() {
        // 2026-01-01T00:00:00Z = 1767225600; if we ever get a value
        // smaller than that, the system clock is plainly broken and
        // we want CI to scream rather than ship a 1970-epoch row.
        let s = now_seconds();
        assert!(s > 1_767_225_600, "system clock looks unset: {s}");
    }
}

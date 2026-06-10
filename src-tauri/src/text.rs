//! Shared text helpers.
//!
//! `&s[..cap]` on a `str` panics when `cap` lands inside a multibyte
//! UTF-8 sequence. Every bounded-scan site that slices raw LLM output
//! (which is routinely Turkish, so multibyte chars are the norm, not
//! the exception) must clamp to a char boundary first — use
//! [`truncate_to_char_boundary`] instead of slicing directly.

/// Largest prefix of `s` that is at most `cap` bytes long and ends on
/// a char boundary. Returns `s` unchanged when it already fits.
pub fn truncate_to_char_boundary(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate `s` to at most `max_chars` Unicode characters. Bounded
/// by `chars()` (not bytes) so the result never splits a multi-byte
/// codepoint. (Consolidates the five identical local copies that
/// used to live in verdict/decision/orchestrator/store-cols/otlp.)
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
    }
}

/// Truncate a string for mailbox `summary` fields — 80 chars with an
/// ellipsis is enough for the mailbox UI to render a recognisable
/// line without overflowing. (Consolidates the identical copies in
/// `commands::swarm::dispatch` and `swarm::brain::prompt`.)
pub fn truncate_for_summary(s: &str) -> String {
    const CAP: usize = 80;
    if s.chars().count() <= CAP {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(CAP).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_to_char_boundary;

    #[test]
    fn short_input_is_returned_unchanged() {
        assert_eq!(truncate_to_char_boundary("abc", 16), "abc");
    }

    #[test]
    fn exact_cap_on_boundary_slices_cleanly() {
        assert_eq!(truncate_to_char_boundary("abcdef", 3), "abc");
    }

    #[test]
    fn multibyte_char_straddling_cap_is_dropped_whole() {
        // 'ü' is 2 bytes; cap lands mid-char → walk back to 1.
        let s = "aüz";
        assert_eq!(truncate_to_char_boundary(s, 2), "a");
    }

    #[test]
    fn multibyte_heavy_input_never_panics_at_any_cap() {
        let s = "üğişçö€😀".repeat(8);
        for cap in 0..=s.len() {
            let t = truncate_to_char_boundary(&s, cap);
            assert!(t.len() <= cap);
            assert!(s.starts_with(t));
        }
    }
}

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

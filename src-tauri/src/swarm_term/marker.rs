//! Routing marker parser.
//!
//! Grammar (v1, single-line form only):
//!   ^>>\s+@([a-z][a-z0-9-]{1,40}):\s+(.+)$
//!
//! The marker must be at column 0 (no leading whitespace) so that
//! quoted output, code blocks, and markdown blockquotes can't
//! false-positive.

use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub target: String,
    pub body: String,
}

fn marker_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^>>\s+@([a-z][a-z0-9-]{1,40}):\s+(.+)$")
            .expect("marker regex compiles")
    })
}

/// Parse a single line. Returns `Some(Marker)` iff the line, with
/// trailing `\r` stripped, matches the grammar verbatim. Leading
/// whitespace disqualifies a line.
pub fn parse_marker_line(line: &str) -> Option<Marker> {
    let line = line.trim_end_matches('\r').trim_end_matches('\n');
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let caps = marker_regex().captures(line)?;
    Some(Marker {
        target: caps.get(1)?.as_str().to_string(),
        body: caps.get(2)?.as_str().trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_simple_marker() {
        let m = parse_marker_line(">> @scout: find the db handler").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "find the db handler");
    }

    #[test]
    fn valid_kebab_target() {
        let m =
            parse_marker_line(">> @backend-builder: implement X").unwrap();
        assert_eq!(m.target, "backend-builder");
        assert_eq!(m.body, "implement X");
    }

    #[test]
    fn valid_multi_colon_body_preserved() {
        let m = parse_marker_line(
            ">> @planner: stage1: scout, stage2: build",
        )
        .unwrap();
        assert_eq!(m.target, "planner");
        assert_eq!(m.body, "stage1: scout, stage2: build");
    }

    #[test]
    fn rejects_leading_whitespace() {
        assert!(parse_marker_line("  >> @scout: hi").is_none());
        assert!(parse_marker_line("\t>> @scout: hi").is_none());
    }

    #[test]
    fn rejects_mid_line_marker() {
        assert!(
            parse_marker_line("some text >> @scout: hi").is_none()
        );
    }

    #[test]
    fn rejects_triple_chevron() {
        assert!(parse_marker_line(">>> @scout: hi").is_none());
    }

    #[test]
    fn rejects_missing_at_sign() {
        assert!(parse_marker_line(">> scout: hi").is_none());
    }

    #[test]
    fn rejects_empty_body() {
        assert!(parse_marker_line(">> @scout: ").is_none());
        assert!(parse_marker_line(">> @scout:").is_none());
    }

    #[test]
    fn rejects_uppercase_target() {
        assert!(parse_marker_line(">> @Scout: hi").is_none());
    }

    #[test]
    fn rejects_target_starts_with_digit() {
        assert!(parse_marker_line(">> @1scout: hi").is_none());
    }

    #[test]
    fn strips_crlf_tail() {
        let m =
            parse_marker_line(">> @scout: find handler\r\n").unwrap();
        assert_eq!(m.body, "find handler");
    }

    #[test]
    fn markdown_blockquote_rejected() {
        assert!(parse_marker_line("> >> @scout: hi").is_none());
    }
}

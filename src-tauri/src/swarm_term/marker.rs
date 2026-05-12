//! Routing marker parser.
//!
//! Canonical grammar (what the persona footer instructs claude to
//! emit, verbatim, at column 0):
//!
//!   `>> @<agent-id>: <body>`
//!
//! claude is a flexible LLM, however, and in practice frequently
//! decorates the marker with markdown — list bullets (`- `, `* `,
//! `1. `), blockquotes (`> `), bold (`**…**`), inline code (`` `…` ``)
//! — or drops the space between `>>` and `@`. A strict regex misses
//! all of those and the route never fires. So this parser runs in
//! two passes:
//!
//!   1. `strip_decorator_prefix` peels common markdown decorators
//!      off the front (and matching trailers off the back) until the
//!      line either starts with `>>` or no more decorator is found.
//!   2. A permissive regex accepts arbitrary whitespace around the
//!      `@`/`:` glue.
//!
//! The column-0 requirement is preserved: callers must `trim_start`
//! before handing the line in, AND any leading decorator must be a
//! known markdown one (regular text never sneaks through). The
//! `markdown_blockquote_rejected` semantic is preserved as
//! `>` -becomes- decorator -strip-, the marker still gets recognised
//! because that's the intent here (claude wraps routing intent in a
//! blockquote when "quoting itself"; the user wants the route to fire).

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
        // Prefix-agnostic form: 0–5 non-whitespace, non-`@`
        // characters before the `@<id>:` glyph. The strict `>>`
        // ASCII chevron is one of many shapes claude can render
        // the marker as in interactive mode — `àáá`, `▶▶▶`, `»»`,
        // `→`, depends on locale/font/its-own-judgement — so we
        // accept anything ≤5 chars and rely on the hierarchy gate
        // + `panes_by_agent` lookup to keep prose mentions of an
        // agent (e.g. `Hi @scout: how are you?`) from blowing up:
        // an unknown / forbidden target is still surfaced as
        // `unknown_target` / `denied` in the overlay, not silently
        // routed.
        //
        // Lazy `{0,5}?` ensures the prefix capture stays minimal
        // so the `@<id>:` group lines up; `[^\s@]` excludes
        // whitespace + `@` so we don't accidentally swallow part
        // of the agent ref.
        Regex::new(r"^([^\s@]{0,5}?)\s*@([a-z][a-z0-9-]{1,40})\s*:\s*(.+?)\s*$")
            .expect("marker regex compiles")
    })
}

/// Regex matching `@<agent-id>:` anywhere in a line — used purely
/// for the near-miss diagnostic in `router::handle_line` so we can
/// log "marker-looking text didn't match the full grammar" when
/// claude phrases things slightly off.
pub fn near_miss_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"@([a-z][a-z0-9-]{1,40}):")
            .expect("near-miss regex compiles")
    })
}

/// Peel one round of common markdown decorators off the front of a
/// line. Returns the (possibly identical) trimmed remainder. The
/// caller loops this until the line stabilises or starts with `>>`.
///
/// Decorators stripped (each followed by at least one space, except
/// the symmetric `**` / `` ` `` pair which strip without a space):
///
///   * `- ` / `* ` / `+ `          — unordered list bullet
///   * `1. ` … `99. `              — ordered list marker
///   * `> `                        — blockquote
///   * `**`                        — bold open / close
///   * `` ` ``                     — inline code open / close
fn strip_decorator_prefix(line: &str) -> &str {
    let s = line.trim_start();

    // `**...**` — bold wrap
    if let Some(rest) = s.strip_prefix("**") {
        return rest;
    }
    // backtick wrap
    if let Some(rest) = s.strip_prefix('`') {
        return rest;
    }
    // blockquote `> ` (also matches plain `>` followed by space-less
    // text, defensive)
    if let Some(rest) = s.strip_prefix("> ") {
        return rest;
    }
    if let Some(rest) = s.strip_prefix('>').filter(|r| !r.starts_with('>')) {
        // `>` followed by non-`>` = blockquote without the space;
        // but DON'T strip the leading `>` of our own `>>` marker.
        return rest;
    }
    // unordered list bullet
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(prefix) {
            return rest;
        }
    }
    // ordered list: `N. ` or `N) ` where N is 1-99
    if let Some(rest) = strip_ordered_list_marker(s) {
        return rest;
    }
    s
}

fn strip_ordered_list_marker(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == 0 || i > 2 {
        // Need 1 or 2 digits; 3+ is not a list marker.
        return None;
    }
    let after_digits = bytes.get(i)?;
    if *after_digits != b'.' && *after_digits != b')' {
        return None;
    }
    let after_punct = bytes.get(i + 1)?;
    if *after_punct != b' ' {
        return None;
    }
    Some(&s[i + 2..])
}

/// Trim trailing markdown closers (matching pair to leading
/// decorator) — `**` and `` ` ``. Only one round; the body itself
/// may not end with these chars after stripping.
fn strip_trailing_decorators(s: &str) -> &str {
    let t = s.trim_end();
    if let Some(rest) = t.strip_suffix("**") {
        return rest.trim_end();
    }
    if let Some(rest) = t.strip_suffix('`') {
        return rest.trim_end();
    }
    t
}

/// Parse a single line. Returns `Some(Marker)` iff the line, after
/// peeling any number of markdown decorators off the front + back
/// and stripping CR/LF tails, matches the (permissive) marker
/// grammar.
///
/// Leading **whitespace** still disqualifies — callers should
/// `trim_start` before invoking. The decorator strip handles
/// non-whitespace markdown prefixes (`- `, `**`, `> `, etc.).
pub fn parse_marker_line(line: &str) -> Option<Marker> {
    let line = line.trim_end_matches('\r').trim_end_matches('\n');
    if line.starts_with(char::is_whitespace) {
        return None;
    }

    // Peel decorator prefixes (bullets, blockquotes, bold,
    // backticks) — claude sometimes wraps the marker in markdown
    // and stripping the outer layer makes the regex line up. Capped
    // at 6 rounds for safety against pathological nesting.
    let mut cur = line;
    for _ in 0..6 {
        let next = strip_decorator_prefix(cur);
        if next.len() == cur.len() {
            break;
        }
        cur = next;
    }
    let cur = strip_trailing_decorators(cur);

    let caps = marker_regex().captures(cur)?;
    // Group 1 is the prefix capture (`>>`, `àáá`, `→`, …). Its only
    // job is to anchor `^([^\s@]{0,5}?)` so the regex doesn't slurp
    // arbitrary mid-line content; the marker semantics live in
    // groups 2 (target) and 3 (body).
    Some(Marker {
        target: caps.get(2)?.as_str().to_string(),
        body: caps.get(3)?.as_str().trim().to_string(),
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
    fn accepts_triple_chevron() {
        // Used to reject. The new prefix-agnostic parser accepts
        // any 0–5 non-whitespace prefix before `@<id>:`, so a
        // stray third `>` (claude occasionally over-emphasises) is
        // fine. Hierarchy + target lookup remain the real gate.
        let m = parse_marker_line(">>> @scout: hi").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hi");
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

    // --- decorator-tolerant parses ---------------------------------
    //
    // These document the formats claude actually emits in the wild
    // (observed in the user's first end-to-end smoke). The strict
    // regex used to reject all of them and routing never fired.

    #[test]
    fn accepts_bullet_dash_prefix() {
        let m = parse_marker_line("- >> @scout: do thing").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "do thing");
    }

    #[test]
    fn accepts_bullet_star_prefix() {
        let m = parse_marker_line("* >> @planner: plan it").unwrap();
        assert_eq!(m.target, "planner");
    }

    #[test]
    fn accepts_ordered_list_prefix() {
        let m = parse_marker_line("1. >> @scout: hello").unwrap();
        assert_eq!(m.target, "scout");
        let m2 = parse_marker_line("12. >> @scout: hello").unwrap();
        assert_eq!(m2.target, "scout");
    }

    #[test]
    fn accepts_bold_wrap() {
        let m = parse_marker_line("**>> @scout: hello**").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hello");
    }

    #[test]
    fn accepts_inline_code_wrap() {
        let m = parse_marker_line("`>> @scout: hello`").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hello");
    }

    #[test]
    fn accepts_combined_bullet_and_bold() {
        let m = parse_marker_line("- **>> @scout: hello**").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hello");
    }

    #[test]
    fn accepts_blockquote_prefix() {
        // claude sometimes wraps routing intent in a markdown
        // blockquote (`> `) — historically rejected because we
        // confused it with a literal user quote. The hierarchy +
        // pane filtering already prevents abuse, so accept it.
        let m = parse_marker_line("> >> @scout: hi").unwrap();
        assert_eq!(m.target, "scout");
    }

    #[test]
    fn accepts_no_space_after_chevron() {
        let m = parse_marker_line(">>@scout: hi").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hi");
    }

    #[test]
    fn accepts_space_before_colon() {
        let m = parse_marker_line(">> @scout : hi").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hi");
    }

    #[test]
    fn accepts_no_space_after_colon() {
        let m = parse_marker_line(">> @scout:hi").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hi");
    }

    #[test]
    fn accepts_extra_internal_whitespace() {
        let m = parse_marker_line(">>   @scout   :   hello").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hello");
    }

    // --- prefix-agnostic acceptance (what the user actually sees) --

    #[test]
    fn accepts_unicode_chevron_prefix_uaaa() {
        // Smoke output observed in the field: claude rendered the
        // marker prefix as Latin-1 accented letters (font fallback
        // for some non-ASCII chevron). Strict `>>` rejected it,
        // routing never fired. Accept now.
        let m = parse_marker_line("àáá @coordinator: Merhaba").unwrap();
        assert_eq!(m.target, "coordinator");
        assert_eq!(m.body, "Merhaba");
    }

    #[test]
    fn accepts_unicode_arrow_prefix() {
        for prefix in ["→", "▶", "▷", "»", "»»", "›", "⇒"] {
            let line = format!("{prefix} @scout: hi");
            let m = parse_marker_line(&line)
                .unwrap_or_else(|| panic!("prefix `{prefix}` rejected"));
            assert_eq!(m.target, "scout");
            assert_eq!(m.body, "hi");
        }
    }

    #[test]
    fn accepts_no_prefix_at_all() {
        // Bare `@<agent>:` at column 0 is legitimate too — claude
        // sometimes drops the chevron entirely.
        let m = parse_marker_line("@planner: outline the steps").unwrap();
        assert_eq!(m.target, "planner");
        assert_eq!(m.body, "outline the steps");
    }

    #[test]
    fn rejects_two_words_before_at() {
        // Two-word prose like `Sıradaki adım @scout: foo` should
        // NOT trigger — the regex only allows ≤5 non-whitespace
        // chars before `@`, so the second whitespace separator
        // breaks the match.
        assert!(parse_marker_line("hello world @scout: hi").is_none());
        assert!(parse_marker_line("Sıradaki adım @scout: hi").is_none());
    }

    #[test]
    fn near_miss_regex_finds_at_target_colon_anywhere() {
        let r = near_miss_regex();
        // Lines that LOOK like they want to route but failed the
        // strict marker — the diagnostic uses this to log a warning.
        assert!(r.is_match("Sending to @scout: please find …"));
        assert!(r.is_match("Sıradaki adım @planner: plan"));
        // Non-marker content with `@` is filtered out by the
        // bracket-followed-by-colon requirement.
        assert!(!r.is_match("emails like me@example.com"));
    }
}

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
        // Prefix-tolerant form: 0–10 chars (including whitespace)
        // before the `@<id>:` glyph. claude renders the `>>`
        // decorator in wildly variable shapes — ASCII `>>`, Unicode
        // arrows `▶▶`, double-chevrons `»»`, `→`, or paired arrows
        // SEPARATED BY A SPACE (`▶ ▶ @target:`, observed verbatim
        // in 2026-05-12 smoke). The prior `[^\s@]{0,5}?` form
        // rejected any decorator with internal whitespace and the
        // route never fired — the near-miss diagnostic captured
        // those misses by the dozen.
        //
        // Lazy `{0,10}?` ensures the prefix capture stays minimal
        // so the `@<id>:` group lines up; `[^@]` only excludes `@`
        // so whitespace inside short decorator chains is OK. The
        // cap of 10 chars (Unicode scalars, not bytes) still
        // blocks long prose intros like `I'll now ask @scout: ...`
        // (13 chars before `@`) from accidentally routing. The
        // hierarchy gate + `panes_by_agent` lookup remain the
        // second line of defence — unknown/forbidden targets land
        // as `unknown_target` / `denied` events, not silent routes.
        Regex::new(r"^([^@]{0,10}?)@([a-z][a-z0-9-]{1,40})\s*:\s*(.+?)\s*$")
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

/// Substring fallback for claude's specific REPL rendering of `>>`
/// as `▎ ▎` (two U+258E LEFT ONE QUARTER BLOCK glyphs separated by
/// a space) when its progress indicator status bar shares the PTY
/// line with the marker via cursor positioning.
///
/// Observed in 2026-05-12 smoke as a recurring near-miss pattern:
///
///   `4*  5✢  6711 tokens)●  ▎ ▎ @orchestrator: Merhaba.`
///   `e✢ N*✶✻✽✻  10s · ↓ 365 tokens)✶●  ▎ ▎ @orchestrator: Mesaj…`
///
/// The column-0 parser can't match because the noise prefix
/// exceeds the 10-char cap. But the `▎ ▎` glyph pair is exclusive
/// to claude's REPL renderer — it does not appear in user prose
/// — so finding it anywhere in the line is a safe second-pass
/// signal that the trailing text is a real marker.
///
/// Anchored at line end (`$`) so it only catches the marker if it
/// sits at the END of the composite line (which is where claude
/// puts the assistant text relative to the status overlay). Lazy
/// body capture `(.+?)` + trailing `\s*$` keeps the body clean.
fn marker_substring_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"▎\s*▎\s*@([a-z][a-z0-9-]{1,40})\s*:\s*(.+?)\s*$")
            .expect("substring marker regex compiles")
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

    // Path 1: column-0 form (the canonical case — claude wrote the
    // marker on its own line, possibly with markdown decoration).
    if !line.starts_with(char::is_whitespace) {
        // Peel decorator prefixes (bullets, blockquotes, bold,
        // backticks) — claude sometimes wraps the marker in
        // markdown and stripping the outer layer makes the regex
        // line up. Capped at 6 rounds for safety against
        // pathological nesting.
        let mut cur = line;
        for _ in 0..6 {
            let next = strip_decorator_prefix(cur);
            if next.len() == cur.len() {
                break;
            }
            cur = next;
        }
        let cur = strip_trailing_decorators(cur);

        if let Some(caps) = marker_regex().captures(cur) {
            // Group 1 is the prefix capture, groups 2 + 3 are the
            // target + body.
            return Some(Marker {
                target: caps.get(2)?.as_str().to_string(),
                body: caps.get(3)?.as_str().trim().to_string(),
            });
        }
    }

    // Path 2: claude REPL status-overlay fallback. The progress
    // indicator gets cursor-positioned ONTO the same PTY line as
    // the marker, so the column-0 form sees a long noise prefix
    // and bails out. Find the `▎ ▎` glyph pair as a substring —
    // unique to claude's renderer, safe from prose false-positives.
    if let Some(caps) = marker_substring_regex().captures(line) {
        return Some(Marker {
            target: caps.get(1)?.as_str().to_string(),
            body: caps.get(2)?.as_str().trim().to_string(),
        });
    }

    None
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
        // Long prose prefix like `Sıradaki adım @scout: foo` should
        // NOT trigger column-0 path — the prefix exceeds the 10-char
        // cap so the regex never matches. The substring fallback
        // (path 2) wouldn't fire either because the prose doesn't
        // contain the `▎ ▎` glyph signature.
        assert!(parse_marker_line("hello world @scout: hi").is_none());
        assert!(parse_marker_line("Sıradaki adım @scout: hi").is_none());
    }

    // --- claude REPL status-overlay substring fallback -------------
    //
    // claude's progress indicator gets cursor-positioned onto the
    // same PTY line as the marker. After strip_ansi the composite
    // looks like `<noise><tokens)● ▎ ▎ @target: body>`. The column-0
    // path bails on the noise prefix; the substring fallback finds
    // the `▎ ▎` glyph pair (U+258E, exclusive to claude's renderer)
    // and extracts the trailing marker.

    #[test]
    fn accepts_claude_status_overlay_with_marker_at_end() {
        let line =
            "4*                  5✢                  6711 tokens)● ▎ ▎ @orchestrator: Merhaba.";
        let m = parse_marker_line(line)
            .expect("substring fallback must catch claude REPL overlay");
        assert_eq!(m.target, "orchestrator");
        assert_eq!(m.body, "Merhaba.");
    }

    #[test]
    fn accepts_claude_overlay_with_token_count_and_spinner() {
        let line =
            "e✢ N*✶✻✽✻                10s · ↓ 365 tokens)✶● ▎ ▎ @orchestrator: Mesajın yarım geldi.";
        let m = parse_marker_line(line).expect("substring fallback");
        assert_eq!(m.target, "orchestrator");
        assert_eq!(m.body, "Mesajın yarım geldi.");
    }

    #[test]
    fn accepts_short_status_overlay() {
        // The minimal observed form: a few padding chars + thinking
        // indicator + the rendered marker. Column-0 path also fails
        // here because `7          thinking● ` is >10 chars.
        let line = "7          thinking● ▎ ▎ @coordinator: Henüz inceleyecek bir builder yok.";
        let m = parse_marker_line(line).expect("substring fallback");
        assert_eq!(m.target, "coordinator");
        assert_eq!(m.body, "Henüz inceleyecek bir builder yok.");
    }

    #[test]
    fn substring_fallback_rejects_html_escaped_marker_in_docs() {
        // Persona excerpts / code-fenced docs render the marker as
        // `&gt;&gt;` (HTML entity) — not the `▎ ▎` glyph pair. The
        // substring fallback must NOT pick these up, otherwise
        // claude documenting its own protocol would emit phantom
        // routes. The current pattern (literal U+258E pair) makes
        // this safe by construction; pin the assertion anyway.
        assert!(
            parse_marker_line("Bittiğinde `&gt;&gt; @frontend-reviewer: özet` yaz")
                .is_none()
        );
        assert!(
            parse_marker_line("- PASS → `&gt;&gt; @coordinator: tüm testler geçti`")
                .is_none()
        );
    }

    #[test]
    fn substring_fallback_does_not_override_column_0_path() {
        // A clean column-0 marker still goes through path 1 (the
        // strict, decorator-tolerant regex), not path 2. Important
        // because path 1 strips trailing markdown (`**`, `` ` ``)
        // which path 2 does not.
        let m = parse_marker_line(">> @scout: hi**").unwrap();
        assert_eq!(m.target, "scout");
        assert_eq!(m.body, "hi"); // trailing `**` peeled by path 1
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

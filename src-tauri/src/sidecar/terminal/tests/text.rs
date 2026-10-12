//! Pure text-helper coverage: DSR-CPR query extraction, terminal
//! line-end trimming, CSI stripping, and UTF-8-safe prefix length.
use super::super::text::{extract_dsr_cpr_queries, strip_csi, trim_terminal_line_end};

#[test]
fn extract_dsr_cpr_extracts_single_query() {
    let mut buf = b"hello\x1b[6nworld".to_vec();
    let n = extract_dsr_cpr_queries(&mut buf);
    assert_eq!(n, 1);
    assert_eq!(buf, b"helloworld");
}

#[test]
fn extract_dsr_cpr_returns_zero_when_absent() {
    let mut buf = b"plain ascii text".to_vec();
    let n = extract_dsr_cpr_queries(&mut buf);
    assert_eq!(n, 0);
    assert_eq!(buf, b"plain ascii text");
}

#[test]
fn extract_dsr_cpr_handles_back_to_back_queries() {
    let mut buf = b"\x1b[6n\x1b[6nfoo".to_vec();
    let n = extract_dsr_cpr_queries(&mut buf);
    assert_eq!(n, 2);
    assert_eq!(buf, b"foo");
}

#[test]
fn extract_dsr_cpr_does_not_consume_partial_match_at_buffer_tail() {
    // A truncated `\x1b[6` (no `n` yet) stays in the buffer so the
    // next read can complete it.
    let mut buf = b"prefix\x1b[6".to_vec();
    let n = extract_dsr_cpr_queries(&mut buf);
    assert_eq!(n, 0);
    assert_eq!(buf, b"prefix\x1b[6");
}

#[test]
fn extract_dsr_cpr_only_matches_exact_query() {
    // Other CSI sequences (cursor save, mode set, …) must not be
    // confused with the DSR-CPR query.
    let mut buf = b"\x1b[?1049h\x1b[2J\x1b[Hbanner".to_vec();
    let n = extract_dsr_cpr_queries(&mut buf);
    assert_eq!(n, 0);
    assert_eq!(buf, b"\x1b[?1049h\x1b[2J\x1b[Hbanner");
}

/// K5 regression: `trim_terminal_line_end` strips trailing CR/LF
/// without touching multi-byte UTF-8 chars or embedded `\r` used
/// for in-place progress updates.
#[test]
fn trim_terminal_line_end_preserves_inline_cr_and_multibyte() {
    // Plain LF.
    assert_eq!(trim_terminal_line_end(b"hello\n"), b"hello");
    // CRLF.
    assert_eq!(trim_terminal_line_end(b"hello\r\n"), b"hello");
    // Bare CR (some terminal apps).
    assert_eq!(trim_terminal_line_end(b"hello\r"), b"hello");
    // Embedded `\r` (progress bar pattern) preserved — only the
    // trailing terminator is trimmed.
    assert_eq!(
        trim_terminal_line_end(b"50%\r60%\n"),
        b"50%\r60%",
    );
    // 3-byte UTF-8 char at end (Greek lowercase delta `δ` = 0xCE 0xB4)
    // followed by LF — bytes are preserved and decode cleanly.
    let bytes = &[0xCEu8, 0xB4u8, b'\n'];
    let trimmed = trim_terminal_line_end(bytes);
    assert_eq!(trimmed, &[0xCEu8, 0xB4u8]);
    assert_eq!(String::from_utf8_lossy(trimmed), "δ");
    // 4-byte UTF-8 char (emoji 🦀 = U+1F980 = F0 9F A6 80) — not
    // mangled by `from_utf8_lossy` because the whole sequence is
    // present.
    let bytes = b"crab \xF0\x9F\xA6\x80\n";
    let trimmed = trim_terminal_line_end(bytes);
    assert_eq!(String::from_utf8_lossy(trimmed), "crab 🦀");
    // All-newline input strips to empty.
    assert_eq!(trim_terminal_line_end(b"\r\n"), b"");
    assert_eq!(trim_terminal_line_end(b""), b"");
}

/// K5 regression: a multi-byte UTF-8 char split across two reader
/// chunks must NOT be replaced with U+FFFD. Before the fix, the
/// `Vec<u8>` accumulator was a `String` and each chunk was decoded
/// in isolation, so the trailing byte of chunk 1 and the leading
/// byte(s) of chunk 2 were each wrapped in U+FFFD. Now the bytes
/// accumulate until a newline is seen and the full sequence is
/// decoded together.
#[test]
fn pending_buffer_concats_split_utf8_before_decode() {
    // Simulate two reads where a 3-byte char (`δ` = CE B4) is
    // split: chunk1 ends with CE, chunk2 starts with B4 then LF.
    let mut pending: Vec<u8> = Vec::new();
    pending.extend_from_slice(b"a"); // chunk 1 first byte
    pending.extend_from_slice(&[0xCE]); // chunk 1 last byte (lead)
    // No newline yet — caller must NOT decode pending.
    assert!(!pending.iter().any(|&b| b == b'\n'));
    pending.extend_from_slice(&[0xB4, b'\n']); // chunk 2

    let idx = pending.iter().position(|&b| b == b'\n').expect("nl");
    let line: Vec<u8> = pending.drain(..=idx).collect();
    let trimmed = trim_terminal_line_end(&line);
    let decoded = String::from_utf8_lossy(trimmed);
    assert_eq!(
        decoded, "aδ",
        "split 3-byte sequence must round-trip without U+FFFD"
    );
}

/// Acceptance: CSI sequences are removed; bare text survives.
#[test]
fn strip_csi_removes_color_and_cursor_codes() {
    // SGR red foreground + reset around "hello"
    let raw = "\x1b[31mhello\x1b[0m";
    assert_eq!(strip_csi(raw), "hello");

    // Cursor home + clear screen
    let raw = "\x1b[H\x1b[2Jcleared";
    assert_eq!(strip_csi(raw), "cleared");

    // OSC 0 (set window title) terminated by BEL
    let raw = "\x1b]0;title\x07rest";
    assert_eq!(strip_csi(raw), "rest");

    // Plain text passes through.
    assert_eq!(strip_csi("plain"), "plain");

    // Multibyte UTF-8 stays intact.
    assert_eq!(strip_csi("süßer Hund 🐶"), "süßer Hund 🐶");

    // Stray ESC directly before a multibyte char must not panic
    // (used to slice mid-char): ESC is dropped, the char survives.
    assert_eq!(strip_csi("\x1bü"), "ü");
    assert_eq!(strip_csi("a\x1b\u{fffd}b"), "a\u{fffd}b");
}

#[test]
fn utf8_safe_prefix_len_holds_back_split_chars() {
    use super::super::text::utf8_safe_prefix_len;

    // Pure ASCII: nothing held back.
    assert_eq!(utf8_safe_prefix_len(b"abc"), 3);
    // Complete multibyte tail: nothing held back.
    assert_eq!(utf8_safe_prefix_len("aü".as_bytes()), 3);
    assert_eq!(utf8_safe_prefix_len("a😀".as_bytes()), 5);
    // Split tail: the partial char's bytes are held back.
    let euro = "€".as_bytes(); // 3 bytes
    let mut buf = b"ab".to_vec();
    buf.extend_from_slice(&euro[..2]);
    assert_eq!(utf8_safe_prefix_len(&buf), 2);
    let smile = "😀".as_bytes(); // 4 bytes
    let mut buf = b"x".to_vec();
    buf.extend_from_slice(&smile[..3]);
    assert_eq!(utf8_safe_prefix_len(&buf), 1);
    // Garbage continuation bytes only: flushed as-is (lossy decode).
    assert_eq!(utf8_safe_prefix_len(&[0x80, 0x80, 0x80, 0x80, 0x80]), 5);
}

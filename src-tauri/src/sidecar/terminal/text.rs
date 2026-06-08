//! Pure byte/text helpers for the PTY read loop.
//!
//! None of these touch pane state — they operate on raw byte buffers
//! and strings, which makes them trivially unit-testable and keeps the
//! reader task in `reader.rs` focused on orchestration.

/// Scan `buf` for every DSR-CPR query (`ESC [ 6 n`, 4 bytes) the child
/// emitted, remove them from the buffer in-place, and return the count.
///
/// The bytes are stripped because they're protocol noise — `claude`
/// doesn't echo them through to subsequent output and we don't want
/// them in the line stream that emit_decoded_line ships to xterm.js.
/// The caller is responsible for writing one `\x1b[1;1R` response per
/// extracted query back through the PTY's master writer.
pub(super) fn extract_dsr_cpr_queries(buf: &mut Vec<u8>) -> usize {
    const QUERY: &[u8] = b"\x1b[6n";
    let mut count = 0;
    let mut from = 0;
    while from + QUERY.len() <= buf.len() {
        if let Some(rel) = buf[from..]
            .windows(QUERY.len())
            .position(|w| w == QUERY)
        {
            let abs = from + rel;
            buf.drain(abs..abs + QUERY.len());
            count += 1;
            from = abs;
        } else {
            break;
        }
    }
    count
}

/// Strip trailing `\r` / `\n` from a raw byte buffer and decode the
/// remainder as lossy UTF-8. Embedded `\r` (carriage-return progress
/// bars) and any control chars within the line are preserved — only
/// the line terminator is stripped.
pub(super) fn trim_terminal_line_end(bytes: &[u8]) -> &[u8] {
    let end = bytes
        .iter()
        .rposition(|&b| b != b'\n' && b != b'\r')
        .map(|i| i + 1)
        .unwrap_or(0);
    &bytes[..end]
}

/// Strip ANSI CSI sequences from `s` for DB storage. The live event
/// payload preserves the original text so xterm.js (WP-W2-08) can
/// render colors and cursor moves correctly.
///
/// Handles the canonical CSI form `ESC [ ... <final byte>` plus the
/// shorter `ESC <single byte>` form (e.g. `ESC c` reset). Anything
/// else is passed through.
pub(super) fn strip_csi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // ESC sequence. Look ahead.
            if let Some(&n) = bytes.get(i + 1) {
                if n == b'[' {
                    // CSI: skip until a final byte in the range 0x40..=0x7e.
                    i += 2;
                    while i < bytes.len() {
                        let c = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                    continue;
                } else if n == b']' {
                    // OSC: skip until BEL (0x07) or ESC \ (string terminator).
                    i += 2;
                    while i < bytes.len() {
                        let c = bytes[i];
                        if c == 0x07 {
                            i += 1;
                            break;
                        }
                        if c == 0x1b {
                            // ESC \ = ST. Consume both.
                            i += 1;
                            if bytes.get(i) == Some(&b'\\') {
                                i += 1;
                            }
                            break;
                        }
                        i += 1;
                    }
                    continue;
                } else {
                    // Single-byte ESC sequence (e.g. `ESC c`).
                    i += 2;
                    continue;
                }
            } else {
                // Trailing ESC with nothing after — drop it.
                i += 1;
                continue;
            }
        }
        // Push the original UTF-8 char boundary safely.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            // Multibyte char: copy one full Unicode scalar.
            let s_rest = &s[i..];
            if let Some(c) = s_rest.chars().next() {
                out.push(c);
                i += c.len_utf8();
            } else {
                i += 1;
            }
        }
    }
    out
}

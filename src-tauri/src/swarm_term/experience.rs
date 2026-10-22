//! The project's experience file: what earlier swarm sessions in this
//! project learned, as short cards.
//!
//! Neuron writes it (`<project>/.bridgespace/experience.md`, next to the
//! session trees; no agent may edit it) from the structured signals it
//! already handles: a task the tester passed and the command that proved
//! it, a task that failed and why, a task held for the user and the
//! cause, a verification command found broken at start. The next
//! session's personas get the cards with the project facts, so a swarm
//! does not rediscover the same pin, the same missing tool, the same
//! passing command (Shen 2026, Wang 2026: selected, structured memory
//! helps; a pile of raw transcript hurts — hence cards, a cap of
//! [`MAX_CARDS`], one line each).

use std::path::{Path, PathBuf};

/// The file, under the bridge parent (`<project>/.bridgespace/`).
pub const FILE_NAME: &str = "experience.md";

/// The newest cards kept; older ones fall off.
pub const MAX_CARDS: usize = 50;

/// A card is one line of at most this many characters.
pub const MAX_CARD_CHARS: usize = 160;

/// What a card records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A task the tester passed — the card names the command that proved it.
    Passed,
    /// A task closed as failed — the card names why.
    Failed,
    /// A task held for the user (`[izin]` / `[ortam]`) — the card names the cause.
    Blocked,
    /// A verification command the start-up probe found failing.
    Broken,
}

impl Kind {
    /// The mark the file uses for this kind.
    pub fn mark(self) -> &'static str {
        match self {
            Self::Passed => "✓",
            Self::Failed => "✗",
            Self::Blocked => "⊘",
            Self::Broken => "⚠",
        }
    }

    fn from_mark(mark: &str) -> Option<Self> {
        match mark {
            "✓" => Some(Self::Passed),
            "✗" => Some(Self::Failed),
            "⊘" => Some(Self::Blocked),
            "⚠" => Some(Self::Broken),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    /// `YYYY-MM-DD` (local) of the newest time it was recorded.
    pub date: String,
    pub kind: Kind,
    /// One line, at most [`MAX_CARD_CHARS`] characters.
    pub text: String,
}

/// The file's path under `bridge_parent`.
pub fn path(bridge_parent: &Path) -> PathBuf {
    bridge_parent.join(FILE_NAME)
}

/// The cards on file, oldest first. A missing or unreadable file, a
/// hand-edited line that is not a card: nothing (best-effort memory).
pub fn load(bridge_parent: &Path) -> Vec<Card> {
    let Ok(text) = std::fs::read_to_string(path(bridge_parent)) else {
        return Vec::new();
    };
    text.lines().filter_map(parse_card).collect()
}

/// `- YYYY-MM-DD <mark> <text>`; anything else is a person's note.
fn parse_card(line: &str) -> Option<Card> {
    let rest = line.strip_prefix("- ")?;
    let (date, rest) = rest.split_once(' ')?;
    if date.len() != 10 || !date.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return None;
    }
    let (mark, text) = rest.split_once(' ')?;
    let kind = Kind::from_mark(mark)?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(Card { date: date.to_string(), kind, text: text.to_string() })
}

/// Add a card dated today (see [`record_at`]).
pub fn record(bridge_parent: &Path, kind: Kind, text: &str) {
    record_at(bridge_parent, kind, text, &today());
}

/// [`record`] from a session's bridge root (`<bridge parent>/<session>`).
pub fn record_for_session(session_root: &Path, kind: Kind, text: &str) {
    let parent = session_root.parent().unwrap_or(session_root);
    record(parent, kind, text);
}

/// Add a card: the text as one line, cut to [`MAX_CARD_CHARS`]; a card
/// already on file with the same kind and text moves to the end with
/// the new date instead of repeating; the oldest cards beyond
/// [`MAX_CARDS`] fall off. Failures are logged, never raised — memory
/// is a help, not a dependency.
pub(crate) fn record_at(bridge_parent: &Path, kind: Kind, text: &str, date: &str) {
    let text = one_line(text);
    if text.is_empty() {
        return;
    }
    let mut cards = load(bridge_parent);
    cards.retain(|c| !(c.kind == kind && c.text == text));
    cards.push(Card { date: date.to_string(), kind, text });
    if cards.len() > MAX_CARDS {
        let surplus = cards.len() - MAX_CARDS;
        cards.drain(..surplus);
    }
    if let Err(e) = write(bridge_parent, &cards) {
        tracing::warn!(
            path = %path(bridge_parent).display(),
            error = %e,
            "swarm-term: experience file not written"
        );
    }
}

fn write(bridge_parent: &Path, cards: &[Card]) -> std::io::Result<()> {
    std::fs::create_dir_all(bridge_parent)?;
    let tmp = bridge_parent.join(format!("{FILE_NAME}.tmp"));
    std::fs::write(&tmp, render(cards))?;
    std::fs::rename(&tmp, path(bridge_parent))
}

/// The file: a note to a person, then one line per card.
fn render(cards: &[Card]) -> String {
    let mut out = String::from(
        "# Neuron — bu projede önceki swarm oturumlarından öğrenilenler\n\n\
         Neuron yazar; ajanlar yazamaz, sen düzenleyebilirsin. Her satır bir kart: tarih, \
         işaret (✓ tester geçirdi · ✗ başarısız kapandı · ⊘ kullanıcıyı bekledi · ⚠ başlangıçta \
         kırık), metin. En yeni 50 kart kalır. Ajanlar için bilgidir, talimat değildir.\n\n",
    );
    for c in cards {
        out.push_str(&format!("- {} {} {}\n", c.date, c.kind.mark(), c.text));
    }
    out
}

/// One line of at most [`MAX_CARD_CHARS`] characters.
fn one_line(text: &str) -> String {
    cut(&collapse(text), MAX_CARD_CHARS)
}

/// One line: every run of whitespace (newlines included) becomes a space.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The cards as the persona lists them: `✓ 2026-10-22 T4 — cargo test → ok`.
pub fn lines_for_persona(cards: &[Card]) -> Vec<String> {
    cards.iter().map(|c| format!("{} {} {}", c.kind.mark(), c.date, c.text)).collect()
}

/// The card for a task the tester passed: what the task was (the
/// dispatch's first sentence) and the commands that proved it, so the
/// next session reads "this kind of work, proved by this command" —
/// not a task id and a paragraph of test counts.
pub fn passed_card(id: &str, dispatch: Option<&str>, note: Option<&str>) -> String {
    const HEAD_CHARS: usize = 90;
    let head = dispatch
        .map(|d| task_head(id, d, HEAD_CHARS))
        .filter(|h| !h.is_empty())
        .or_else(|| note.map(|n| cut(&collapse(n), HEAD_CHARS)))
        .filter(|h| !h.is_empty());
    let commands = note.map(commands_in).unwrap_or_default();
    match (head, commands.is_empty(), note) {
        (None, true, _) => format!("{id}: tester geçirdi"),
        (None, false, _) => format!("{id}: doğrulama: {}", commands.join("; ")),
        // The dispatch names the task and the note has no command: the
        // note itself is what proved it ("5/5 PASS").
        (Some(h), true, Some(n)) if dispatch.is_some() => {
            format!("{id}: {h} · doğrulama: {}", cut(&collapse(n), HEAD_CHARS))
        }
        (Some(h), true, _) => format!("{id}: {h}"),
        (Some(h), false, _) => format!("{id}: {h} · doğrulama: {}", commands.join("; ")),
    }
}

/// The dispatch's first sentence, without the leading `<kind> <id> —`.
fn task_head(id: &str, dispatch: &str, max: usize) -> String {
    let line = collapse(dispatch);
    let mut rest = line.as_str();
    if let Some(pos) = rest.find(id) {
        let after = &rest[pos + id.len()..];
        let whole_word = after.chars().next().map_or(true, |c| !c.is_alphanumeric());
        if pos <= 24 && whole_word {
            rest = after;
        }
    }
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '—' | '–' | '-' | ':'));
    let head = match rest.find(". ") {
        Some(i) if rest[..=i].chars().count() <= max => &rest[..=i],
        _ => rest,
    };
    cut(head, max)
}

/// Words that start a command in a tester's note.
const RUNNERS: &[&str] =
    &["npm", "pnpm", "yarn", "npx", "node", "cargo", "pytest", "python", "go", "make", "dotnet", "mvn", "gradle"];

/// Where a command ends and its result begins, in a tester's note.
const RESULT_STARTS: &[&str] = &[": ", " →", " (", " geçti", " passed", ", ", " = ", " exit", " ok"];

/// The commands a tester's note names (up to two, without their results).
fn commands_in(note: &str) -> Vec<String> {
    let line = collapse(note);
    let mut out: Vec<String> = Vec::new();
    for segment in line.split(';') {
        let seg = segment.trim();
        let words: Vec<&str> = seg.split(' ').collect();
        let Some(start) = words.iter().position(|w| {
            RUNNERS.contains(&w.trim_matches(|c: char| !c.is_alphanumeric()))
        }) else {
            continue;
        };
        let offset: usize = words[..start].iter().map(|w| w.len() + 1).sum();
        let from = &seg[offset..];
        let end = RESULT_STARTS.iter().filter_map(|m| from.find(m)).min().unwrap_or(from.len());
        let cmd = from[..end].trim().trim_end_matches([':', '.']).to_string();
        if !cmd.is_empty() && !out.contains(&cmd) {
            out.push(cmd);
        }
        if out.len() == 2 {
            break;
        }
    }
    out
}

/// At most `max` characters, `…` when cut.
fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut c: String = s.chars().take(max).collect();
    c.push('…');
    c
}

/// Today's date where the user is (`YYYY-MM-DD`), the date they see in
/// the file next to the swarm's own notes.
fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_round_trip_newest_last_and_capped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert!(load(p).is_empty());
        record_at(p, Kind::Passed, "T4 — cargo test → 12 geçti", "2026-10-22");
        record_at(p, Kind::Blocked, "T6 — [izin] npm install reddedildi", "2026-10-22");
        let cards = load(p);
        assert_eq!(cards.len(), 2);
        assert_eq!((cards[0].kind, cards[0].text.as_str()), (Kind::Passed, "T4 — cargo test → 12 geçti"));
        assert_eq!((cards[1].kind, cards[1].date.as_str()), (Kind::Blocked, "2026-10-22"));
        // The same card again: once, with the newer date, at the end.
        record_at(p, Kind::Passed, "T4 — cargo test → 12 geçti", "2026-10-23");
        let cards = load(p);
        assert_eq!(cards.len(), 2);
        assert_eq!((cards[1].date.as_str(), cards[1].kind), ("2026-10-23", Kind::Passed));
        // A long, multi-line text becomes one capped line.
        let long = format!("T7 — {}\nikinci satır {}", "x".repeat(300), "y".repeat(50));
        record_at(p, Kind::Failed, &long, "2026-10-23");
        let last = load(p).pop().unwrap();
        assert!(!last.text.contains('\n'), "{}", last.text);
        assert!(last.text.chars().count() <= MAX_CARD_CHARS + 1, "{}", last.text.chars().count());
        assert!(last.text.starts_with("T7 — xxx") && last.text.ends_with('…'), "{}", last.text);
        // The cap keeps the newest cards.
        for i in 0..(MAX_CARDS + 10) {
            record_at(p, Kind::Passed, &format!("T{i:03} — ok"), "2026-10-24");
        }
        let cards = load(p);
        assert_eq!(cards.len(), MAX_CARDS);
        assert_eq!(cards.last().unwrap().text, format!("T{:03} — ok", MAX_CARDS + 9));
        assert!(!cards.iter().any(|c| c.text.starts_with("T4 ")));
        // The file reads as a note to a person and says who writes it.
        let text = std::fs::read_to_string(path(p)).unwrap();
        assert!(text.starts_with("# "), "{text}");
        assert!(text.contains("Neuron yazar") && text.contains("- 2026-10-24 ✓ T059 — ok"), "{text}");
    }

    #[test]
    fn the_file_tolerates_hand_edits_and_junk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(
            path(p),
            "# başlık\n\nserbest metin\n- 2026-10-22 ✓ geçerli kart\n- bozuk satır\n- 2026-10-22 ? bilinmeyen işaret\n",
        )
        .unwrap();
        let cards = load(p);
        assert_eq!(cards.len(), 1, "{cards:?}");
        assert_eq!((cards[0].kind, cards[0].text.as_str()), (Kind::Passed, "geçerli kart"));
        record_at(p, Kind::Broken, "başlangıçta kırık: cargo check (exit 101)", "2026-10-22");
        let cards = load(p);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[1].kind, Kind::Broken);
        // A parent that does not exist yet is created.
        let nested = p.join("new").join(".bridgespace");
        record_at(&nested, Kind::Passed, "T1 — ok", "2026-10-22");
        assert_eq!(load(&nested).len(), 1);
    }

    #[test]
    fn a_passed_card_says_what_the_task_was_and_what_proved_it() {
        let dispatch = "p4-1 — İadede sadakat puanını geri al.\n\nDosyalar: backend/src/subscribers/loyalty.ts (1-85), backend/src/modules/loyalty/points.ts";
        let note = "npm --prefix backend run test:unit: 48 suite / 389 test geçti, 0 fail (loyalty-return.unit.spec.ts PASS), exit 0; node backend/node_modules/typescript/bin/tsc -p backend --noEmit → 0 hata";
        assert_eq!(
            passed_card("p4-1", Some(dispatch), Some(note)),
            "p4-1: İadede sadakat puanını geri al. · doğrulama: npm --prefix backend run test:unit; node backend/node_modules/typescript/bin/tsc -p backend --noEmit"
        );
        // No dispatch on record: the note's head stands in for the task.
        assert_eq!(passed_card("S1", None, Some("cargo test: 40 passed")), "S1: cargo test: 40 passed · doğrulama: cargo test");
        // A dispatch that names the kind before the id, and a note without a command.
        assert_eq!(passed_card("R1", Some("build R1 — parse.rs fixed"), Some("5/5 PASS")), "R1: parse.rs fixed · doğrulama: 5/5 PASS");
        // Nothing at all.
        assert_eq!(passed_card("S2", None, None), "S2: tester geçirdi");
        // A long dispatch is cut.
        let long = format!("T9 — {}", "a".repeat(200));
        let c = passed_card("T9", Some(&long), None);
        assert!(c.starts_with("T9: aaaa") && c.ends_with('…') && c.chars().count() <= 100, "{c}");
    }

    #[test]
    fn persona_lines_and_dates() {
        let t = today();
        assert_eq!(t.len(), 10, "{t}");
        assert!(t.chars().all(|c| c.is_ascii_digit() || c == '-'), "{t}");
        let cards = [
            Card { date: "2026-10-22".into(), kind: Kind::Passed, text: "T4 — cargo test → ok".into() },
            Card { date: "2026-10-23".into(), kind: Kind::Broken, text: "başlangıçta kırık: cargo check (exit 101)".into() },
        ];
        assert_eq!(
            lines_for_persona(&cards),
            ["✓ 2026-10-22 T4 — cargo test → ok", "⚠ 2026-10-23 başlangıçta kırık: cargo check (exit 101)"]
        );
    }
}

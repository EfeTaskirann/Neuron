//! Routing service — listens on every `panes:{pane_id}:line` event,
//! parses for `>> @agent:` markers, enforces the hierarchy, and pastes
//! the message into the target pane with a `— from @<src>` signature.
//!
//! Emits a `swarm-term:route` Tauri event for each routing attempt
//! (`outcome` = `"ok"` | `"denied"` | `"unknown_target"`) so the UI
//! overlay can render a live timeline.
//!
//! Listener lifetime is bound to the active session. `install` returns
//! the `EventId`s so `TerminalSwarmRegistry::stop` can unlisten when
//! the session ends.
//!
//! ## Pure-vs-IO split
//!
//! `handle_line` is a thin IO shell that strips ANSI, splits on `\n`,
//! and for each candidate row calls `decide_route` — a side-effect-
//! free function that returns a `RouteDecision`. The shell then
//! translates the decision into the appropriate `write_to_pane` /
//! `app.emit` side-effects. The split exists so the routing brain
//! (marker parse → dedupe → hierarchy check → target lookup) is
//! exhaustively unit-testable without a Tauri runtime or a live PTY.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, EventId, Listener, Manager, Runtime};

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::{allowed_for, is_allowed};
use crate::swarm_term::marker::{near_miss_regex, parse_marker_line};

/// Dedupe window covers the rapid repaint loops claude's TUI runs
/// while streaming an assistant token. A marker line gets repainted
/// many times in 100–800 ms as the response paints in; we want one
/// route fire, not dozens. 1500 ms is comfortably wider than the
/// observed paint cadence.
const DEDUPE_WINDOW_MS: u64 = 1500;

/// Per source-pane LRU of the most recently routed `(target, body)`
/// hash. Keyed by the route content rather than the raw line bytes so
/// claude's repaint loop — which rewrites the same row with shifting
/// SGR / cursor preludes — does not slip past dedupe with a fresh hash
/// every frame.
#[derive(Default)]
pub(crate) struct DedupeCell {
    last: HashMap<String, (u64, Instant)>,
}

/// Outcome of running a single candidate line through the routing
/// brain. Pure data — the IO layer translates it into pane writes and
/// Tauri event emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RouteDecision {
    /// Line had no marker (or only a marker that's been recently
    /// routed and is still inside the dedupe window). No-op.
    NoOp,
    /// `>> @<target>:` named a target that's not in the active
    /// session's `panes_by_agent`. The shell writes a notice back to
    /// the source pane and emits a `unknown_target` event.
    UnknownTarget {
        source: String,
        target: String,
        body: String,
    },
    /// Hierarchy graph forbids `source → target`. The shell writes a
    /// `route denied` notice back to source and emits `denied`.
    Denied {
        source: String,
        target: String,
        body: String,
        allowed: Vec<String>,
    },
    /// Marker is valid and the route is permitted. The shell pastes
    /// the signed body into `target_pane` and emits `ok`.
    Route {
        source: String,
        target: String,
        target_pane: String,
        body: String,
    },
}

/// Install one Tauri listener per source pane. Closures capture the
/// agent ↔ pane lookup map (cloned, immutable for the session
/// lifetime), the shared dedupe state, the terminal registry for
/// `write_to_pane`, and the AppHandle for emitting `swarm-term:route`.
pub fn install<R: Runtime>(
    app: AppHandle<R>,
    panes_by_agent: HashMap<String, String>,
) -> Vec<EventId> {
    let dedupe = Arc::new(Mutex::new(DedupeCell::default()));
    let panes_by_agent = Arc::new(panes_by_agent);
    let registry = app.state::<TerminalRegistry>().inner().clone();

    let mut ids: Vec<EventId> = Vec::with_capacity(panes_by_agent.len());
    for (source_agent_id, source_pane_id) in panes_by_agent.iter() {
        let event_name = format!("panes:{source_pane_id}:line");
        let panes_by_agent_c = Arc::clone(&panes_by_agent);
        let registry_c = registry.clone();
        let app_c = app.clone();
        let dedupe_c = Arc::clone(&dedupe);
        let source_pane_id_c = source_pane_id.clone();
        let source_agent_id_c = source_agent_id.clone();

        let id = app.listen(event_name, move |event| {
            let payload_str = event.payload();
            let text = match serde_json::from_str::<Value>(payload_str)
                .ok()
                .and_then(|v| {
                    v.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                }) {
                Some(t) => t,
                None => return,
            };
            // Live `panes:{id}:line` payloads carry RAW text (ANSI
            // escapes intact) so the frontend xterm can render
            // colour. Strip them here before marker parsing so a
            // coloured `\x1b[34m>> @scout: ...` still routes.
            let stripped = strip_ansi(&text);
            handle_line(
                &stripped,
                &source_agent_id_c,
                &source_pane_id_c,
                &panes_by_agent_c,
                &registry_c,
                &app_c,
                &dedupe_c,
            );
        });
        ids.push(id);
    }
    ids
}

fn handle_line<R: Runtime>(
    text: &str,
    source_agent_id: &str,
    source_pane_id: &str,
    panes_by_agent: &Arc<HashMap<String, String>>,
    registry: &TerminalRegistry,
    app: &AppHandle<R>,
    dedupe: &Arc<Mutex<DedupeCell>>,
) {
    // The reader splits the PTY byte stream on raw `\n`, but claude's
    // interactive TUI paints by absolute cursor positioning — many
    // logical rows can be packed into a single `\n`-terminated event.
    // `strip_ansi` converts cursor jumps to `\n` and cursor-right to
    // spaces, turning the TUI dump back into a readable text stream;
    // here we re-split on `\n` and route every marker we find.
    // Routing every marker (not just the first) is critical: claude
    // can dispatch to two specialists in one response (`>> @scout: …`
    // then `>> @planner: …` on the next row) and both need to fire.
    for seg in text.split('\n') {
        let trimmed = seg.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let decision = decide_route(
            trimmed,
            source_agent_id,
            source_pane_id,
            panes_by_agent,
            dedupe,
            Instant::now(),
        );
        // Near-miss diagnostic: a segment that names a known agent
        // via `@<id>:` but didn't fully parse to a marker is almost
        // certainly claude trying to route but phrasing it in prose
        // ("then I'll dispatch to @scout: do thing"). Log + emit a
        // `near_miss` route event so the user can see WHY their
        // route didn't fire instead of staring at silence.
        if matches!(decision, RouteDecision::NoOp) {
            if let Some(caps) = near_miss_regex().captures(trimmed) {
                let candidate = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                if panes_by_agent.contains_key(candidate) {
                    tracing::warn!(
                        source = %source_agent_id,
                        candidate = %candidate,
                        line = %truncate(trimmed, 200),
                        "swarm-term: near-miss — `@{candidate}:` seen but marker grammar didn't match (claude probably phrased it in prose; route NOT fired)"
                    );
                    let _ = app.emit(
                        "swarm-term:route",
                        json!({
                            "source": source_agent_id,
                            "target": candidate,
                            "body": truncate(trimmed, 240),
                            "outcome": "near_miss",
                        }),
                    );
                }
            }
        }
        apply_decision(decision, registry, app);
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Pure routing brain — parse → dedupe → hierarchy/target check.
///
/// Returns a `RouteDecision` describing what (if anything) the IO
/// layer should do. No side effects: dedupe state is mutated through
/// the `Mutex` reference, but that's bookkeeping internal to the
/// brain and is the only mutation visible to callers.
pub(crate) fn decide_route(
    line: &str,
    source_agent_id: &str,
    source_pane_id: &str,
    panes_by_agent: &HashMap<String, String>,
    dedupe: &Mutex<DedupeCell>,
    now: Instant,
) -> RouteDecision {
    let Some(marker) = parse_marker_line(line) else {
        return RouteDecision::NoOp;
    };
    let target_agent_id = marker.target.clone();
    let body = marker.body.clone();

    // Dedupe on (target, body) — claude's repaint loop rewrites the
    // same row many times in a few hundred ms; we want one route
    // fire, not dozens. Keying on the marker content (not the raw
    // line text) lets the changing SGR / cursor preludes pass through
    // without minting new hashes.
    let marker_hash = fast_hash(&format!("{target_agent_id}:{body}"));
    {
        let mut d = dedupe.lock().expect("dedupe poisoned");
        if let Some((prev_hash, prev_time)) =
            d.last.get(source_pane_id).copied()
        {
            if prev_hash == marker_hash
                && now.duration_since(prev_time)
                    < Duration::from_millis(DEDUPE_WINDOW_MS)
            {
                tracing::debug!(
                    source = %source_agent_id,
                    target = %target_agent_id,
                    "swarm-term: dedupe suppressed repeat marker"
                );
                return RouteDecision::NoOp;
            }
        }
        d.last
            .insert(source_pane_id.to_string(), (marker_hash, now));
    }

    tracing::info!(
        source = %source_agent_id,
        target = %target_agent_id,
        body_len = body.len(),
        "swarm-term: marker parsed"
    );

    let Some(target_pane_id) = panes_by_agent.get(&target_agent_id).cloned()
    else {
        return RouteDecision::UnknownTarget {
            source: source_agent_id.to_string(),
            target: target_agent_id,
            body,
        };
    };

    if !is_allowed(source_agent_id, &target_agent_id) {
        let allowed: Vec<String> = allowed_for(source_agent_id)
            .iter()
            .map(|s| s.to_string())
            .collect();
        return RouteDecision::Denied {
            source: source_agent_id.to_string(),
            target: target_agent_id,
            body,
            allowed,
        };
    }

    RouteDecision::Route {
        source: source_agent_id.to_string(),
        target: target_agent_id,
        target_pane: target_pane_id,
        body,
    }
}

fn apply_decision<R: Runtime>(
    decision: RouteDecision,
    registry: &TerminalRegistry,
    app: &AppHandle<R>,
) {
    match decision {
        RouteDecision::NoOp => {}
        RouteDecision::UnknownTarget { source, target, body } => {
            // `target_lookup_failed` instead of the agent name —
            // we don't know the source pane here, so this notice
            // path is wired through the source's pane_id lookup via
            // `panes_by_agent`. Done in the IO layer by re-deriving
            // from `source`. We don't write a notice to source for
            // unknown-target because the marker text already painted
            // in the pane; the overlay's red row is sufficient
            // feedback. Emitting `unknown_target` is what drives the
            // RoutingOverlay update.
            let _ = app.emit(
                "swarm-term:route",
                json!({
                    "source": source,
                    "target": target,
                    "body": body,
                    "outcome": "unknown_target",
                }),
            );
        }
        RouteDecision::Denied { source, target, body, allowed } => {
            let allowed_list = if allowed.is_empty() {
                "(yok)".to_string()
            } else {
                allowed.join(", ")
            };
            tracing::info!(
                source = %source,
                target = %target,
                allowed = %allowed_list,
                "swarm-term: route denied"
            );
            let _ = app.emit(
                "swarm-term:route",
                json!({
                    "source": source,
                    "target": target,
                    "body": body,
                    "outcome": "denied",
                    "allowed": allowed,
                }),
            );
        }
        RouteDecision::Route { source, target, target_pane, body } => {
            let signed = format_routed_message(&body, &source);
            tracing::info!(
                source = %source,
                target = %target,
                target_pane = %target_pane,
                bytes = signed.len(),
                "swarm-term: route firing"
            );
            let registry_w = registry.clone();
            let target_w = target_pane.clone();
            let target_label = target.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) =
                    registry_w.write_to_pane(&target_w, signed.as_bytes()).await
                {
                    tracing::warn!(
                        target_pane = %target_w,
                        target_agent = %target_label,
                        error = %e,
                        "swarm-term: route write failed"
                    );
                }
            });
            let _ = app.emit(
                "swarm-term:route",
                json!({
                    "source": source,
                    "target": target,
                    "body": body,
                    "outcome": "ok",
                }),
            );
        }
    }
}

/// Wire-format the routed payload that's pasted into the target
/// pane's stdin.
///
/// The previous (broken) format was `{body}\r\n\r\n— from …\r`: claude's
/// REPL treats every `\r` (CR) as an Enter keystroke that submits the
/// current input buffer, so the embedded `\r\n\r\n` SPLIT the message
/// into three sequential submits (body, empty, signature) — the receiver
/// saw two malformed prompts back-to-back instead of one coherent
/// instruction.
///
/// The fix is to wrap the whole message in xterm **bracketed paste**
/// markers (`\x1b[200~ … \x1b[201~`), use `\n` (not `\r\n`) for the
/// internal line break, and submit with one trailing `\r` outside the
/// paste block. claude's REPL — which enables `?2004h` on startup —
/// then accepts the whole thing as a single multi-line message and
/// submits it once.
pub(crate) fn format_routed_message(body: &str, source_agent: &str) -> String {
    format!(
        "\x1b[200~{body}\n\n— from @{source_agent} [routed by Neuron]\x1b[201~\r"
    )
}

fn fast_hash(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// ANSI/CSI stripper tuned for claude's interactive TUI output.
///
/// The previous (W5-era) version discarded every CSI sequence
/// silently, which destroyed the spacing and row layout of claude's
/// REPL paints: cursor-forward escapes (`ESC [ N C`) were used as
/// inter-word spacers and absolute-position jumps (`ESC [ r ; c H`)
/// terminated logical rows. After a naive strip, an output like
/// `\e[6;1H>>\e[1C@scout:\e[1Cfind` collapsed to `>>@scout:find` —
/// the marker regex would not match because `>>` and `@` had no
/// whitespace between them, and the line was no longer column-0.
///
/// This version preserves the structural intent:
///
///   * `ESC [ N C`  (Cursor Forward)            → `N` spaces (cap 200)
///   * `ESC [ N B`  (Cursor Down)               → `N` newlines (cap 20)
///   * `ESC [ N E`  (Cursor Next Line)          → `N` newlines (cap 20)
///   * `ESC [ H` / `ESC [ r ; c H` / `… f`      → newline (row jump)
///   * `ESC E` (NEL) / `ESC D` (IND)            → newline
///   * Other CSI/OSC/short-ESC                  → stripped silently
///
/// The transform is one-pass and does not allocate beyond `out`.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b != 0x1b {
            out.push(b as char);
            i += 1;
            continue;
        }
        let Some(&next) = bytes.get(i + 1) else {
            i += 1;
            continue;
        };
        match next {
            b'[' => {
                // CSI: collect params until final byte (0x40..=0x7e).
                let start = i + 2;
                let mut j = start;
                while j < bytes.len() {
                    let c = bytes[j];
                    if (0x40..=0x7e).contains(&c) {
                        break;
                    }
                    j += 1;
                }
                if j >= bytes.len() {
                    // Incomplete CSI at buffer tail — drop.
                    break;
                }
                let final_b = bytes[j];
                let params = &bytes[start..j];
                match final_b {
                    b'C' => {
                        let n = first_uint_or(params, 1).min(200);
                        for _ in 0..n {
                            out.push(' ');
                        }
                    }
                    b'B' | b'E' => {
                        let n = first_uint_or(params, 1).min(20);
                        for _ in 0..n {
                            out.push('\n');
                        }
                    }
                    b'H' | b'f' => {
                        // Avoid stacking redundant newlines when claude
                        // jumps to the same column twice in a row.
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    _ => { /* SGR, mode-set, erase, …: strip */ }
                }
                i = j + 1;
            }
            b']' => {
                // OSC ESC ] … BEL | ESC \
                i += 2;
                while i < bytes.len() {
                    let c = bytes[i];
                    if c == 0x07 {
                        i += 1;
                        break;
                    }
                    if c == 0x1b {
                        i += 1;
                        if bytes.get(i) == Some(&b'\\') {
                            i += 1;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'E' | b'D' => {
                // NEL / IND — explicit line break in the 7-bit form.
                out.push('\n');
                i += 2;
            }
            _ => {
                // Other short ESC: skip 2 bytes.
                i += 2;
            }
        }
    }
    out
}

/// Parse the leading ASCII-digit run from a CSI-param byte slice and
/// return it as u32, falling back to `default` if no digits precede
/// the first non-digit / separator byte. Used by `strip_ansi` to read
/// the `N` in `ESC [ N C` and `ESC [ N B` without pulling in `regex`.
fn first_uint_or(bytes: &[u8], default: u32) -> u32 {
    let mut n: u32 = 0;
    let mut any = false;
    for &b in bytes {
        if b.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((b - b'0') as u32);
            any = true;
        } else {
            break;
        }
    }
    if any {
        n
    } else {
        default
    }
}

/// Tear down listeners installed by `install`.
pub fn uninstall<R: Runtime>(app: &AppHandle<R>, ids: Vec<EventId>) {
    for id in ids {
        app.unlisten(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dedupe() -> Mutex<DedupeCell> {
        Mutex::new(DedupeCell::default())
    }

    fn full_panes_map() -> HashMap<String, String> {
        // Every agent gets a synthetic pane id; the router doesn't
        // care that no PTY backs them — `decide_route` is pure.
        let mut m = HashMap::new();
        for &a in crate::swarm_term::hierarchy::AGENT_IDS {
            m.insert(a.to_string(), format!("p-{a}"));
        }
        m
    }

    #[test]
    fn fast_hash_is_stable() {
        assert_eq!(fast_hash("x"), fast_hash("x"));
        assert_ne!(fast_hash("x"), fast_hash("y"));
    }

    #[test]
    fn strip_ansi_removes_csi_color() {
        assert_eq!(strip_ansi("\x1b[34mhello\x1b[0m"), "hello");
    }

    #[test]
    fn strip_ansi_passes_plain_text_through() {
        assert_eq!(strip_ansi(">> @scout: hi"), ">> @scout: hi");
    }

    #[test]
    fn strip_ansi_then_marker_parse_round_trip() {
        let raw = "\x1b[1m>> @planner:\x1b[0m do thing";
        let stripped = strip_ansi(raw);
        let m = parse_marker_line(&stripped).unwrap();
        assert_eq!(m.target, "planner");
        assert!(m.body.contains("do thing"));
    }

    // --- decide_route: pure brain ----------------------------------

    #[test]
    fn decide_route_no_marker_is_noop() {
        let panes = full_panes_map();
        let d = dedupe();
        let r = decide_route(
            "just some claude output",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            Instant::now(),
        );
        assert_eq!(r, RouteDecision::NoOp);
    }

    #[test]
    fn decide_route_happy_path_returns_route() {
        let panes = full_panes_map();
        let d = dedupe();
        let r = decide_route(
            ">> @scout: find the db layer",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            Instant::now(),
        );
        match r {
            RouteDecision::Route { source, target, target_pane, body } => {
                assert_eq!(source, "orchestrator");
                assert_eq!(target, "scout");
                assert_eq!(target_pane, "p-scout");
                assert_eq!(body, "find the db layer");
            }
            other => panic!("expected Route, got {other:?}"),
        }
    }

    #[test]
    fn decide_route_unknown_target() {
        let panes = full_panes_map();
        let d = dedupe();
        let r = decide_route(
            ">> @nobody: hello",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            Instant::now(),
        );
        match r {
            RouteDecision::UnknownTarget { source, target, .. } => {
                assert_eq!(source, "orchestrator");
                assert_eq!(target, "nobody");
            }
            other => panic!("expected UnknownTarget, got {other:?}"),
        }
    }

    #[test]
    fn decide_route_hierarchy_forbidden() {
        // scout → backend-builder is not allowed (scout can only talk
        // to coordinator/orchestrator).
        let panes = full_panes_map();
        let d = dedupe();
        let r = decide_route(
            ">> @backend-builder: do thing",
            "scout",
            "p-scout",
            &panes,
            &d,
            Instant::now(),
        );
        match r {
            RouteDecision::Denied { source, target, allowed, .. } => {
                assert_eq!(source, "scout");
                assert_eq!(target, "backend-builder");
                assert!(allowed.contains(&"coordinator".to_string()));
                assert!(allowed.contains(&"orchestrator".to_string()));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn decide_route_dedupes_same_marker_within_window() {
        let panes = full_panes_map();
        let d = dedupe();
        let now = Instant::now();
        let r1 = decide_route(
            ">> @scout: do thing",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now,
        );
        assert!(matches!(r1, RouteDecision::Route { .. }));
        // Repaint of the same row 100 ms later — must NOT re-fire.
        let r2 = decide_route(
            ">> @scout: do thing",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now + Duration::from_millis(100),
        );
        assert_eq!(r2, RouteDecision::NoOp);
    }

    #[test]
    fn decide_route_does_not_dedupe_different_body() {
        let panes = full_panes_map();
        let d = dedupe();
        let now = Instant::now();
        let r1 = decide_route(
            ">> @scout: first task",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now,
        );
        assert!(matches!(r1, RouteDecision::Route { .. }));
        let r2 = decide_route(
            ">> @scout: second task",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now + Duration::from_millis(100),
        );
        assert!(matches!(r2, RouteDecision::Route { .. }));
    }

    #[test]
    fn decide_route_re_fires_after_dedupe_window() {
        let panes = full_panes_map();
        let d = dedupe();
        let now = Instant::now();
        let _ = decide_route(
            ">> @scout: do thing",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now,
        );
        // 2 s later — outside DEDUPE_WINDOW_MS — same marker MUST fire.
        let r2 = decide_route(
            ">> @scout: do thing",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now + Duration::from_millis(2_000),
        );
        assert!(matches!(r2, RouteDecision::Route { .. }));
    }

    #[test]
    fn decide_route_dedupe_is_per_source_pane() {
        // Two source panes emitting identical markers must both fire.
        let panes = full_panes_map();
        let d = dedupe();
        let now = Instant::now();
        let r1 = decide_route(
            ">> @scout: do thing",
            "orchestrator",
            "p-orchestrator",
            &panes,
            &d,
            now,
        );
        assert!(matches!(r1, RouteDecision::Route { .. }));
        let r2 = decide_route(
            ">> @scout: do thing",
            "coordinator",
            "p-coordinator",
            &panes,
            &d,
            now + Duration::from_millis(100),
        );
        assert!(matches!(r2, RouteDecision::Route { .. }));
    }

    // --- format_routed_message: wire format ------------------------

    #[test]
    fn format_uses_bracketed_paste_and_single_submit_cr() {
        let s = format_routed_message("body", "scout");
        // Starts with bracketed-paste prologue, ends with `\r` outside
        // the paste block — the previous broken format embedded `\r`s
        // INSIDE the body, splitting the message into multiple submits.
        assert!(s.starts_with("\x1b[200~"));
        assert!(s.ends_with("\x1b[201~\r"));
        // Exactly one `\r` (the terminal Enter), and zero before the
        // closing bracketed-paste marker.
        let cr_count = s.matches('\r').count();
        assert_eq!(
            cr_count, 1,
            "routed payload must have exactly one trailing CR; \
             extra CRs are interpreted by claude REPL as message \
             submits and split the message"
        );
    }

    #[test]
    fn format_includes_body_and_signature() {
        let s = format_routed_message("do the thing", "orchestrator");
        assert!(s.contains("do the thing"));
        assert!(s.contains("— from @orchestrator [routed by Neuron]"));
    }

    #[test]
    fn format_preserves_body_multiline_via_lf_only() {
        // If a body ever contains `\n` (defensive — marker parsing
        // strips it today), it should remain `\n` and not become `\r`.
        let s = format_routed_message("line1\nline2", "scout");
        assert!(s.contains("line1\nline2"));
    }

    // --- end-to-end Tauri event wiring -----------------------------
    //
    // Below tests exercise `install()` under the Tauri MockRuntime to
    // prove the actual `app.listen("panes:{id}:line")` → `decide_route`
    // → `app.emit("swarm-term:route")` chain fires. Without these the
    // routing brain could be 100 % correct in isolation while the
    // listener never received events (the failure mode we shipped
    // until this fix). `write_to_pane` is expected to error in these
    // tests because no real PaneState is registered — that's fine,
    // the route-firing decision and the `swarm-term:route` emit
    // happen before the (spawned, fire-and-forget) write attempt.

    use serde::Deserialize;
    use std::sync::Mutex as StdMutex;
    use tauri::{Emitter, Listener};

    #[derive(Debug, Clone, Deserialize)]
    struct CapturedRoute {
        source: String,
        target: String,
        body: String,
        outcome: String,
    }

    fn mock_app_with_terminal_registry()
    -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .manage(crate::sidecar::terminal::TerminalRegistry::new())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app build")
    }

    /// Spin up the listener chain, fire a `panes:{src}:line` event with
    /// a valid marker, and capture the `swarm-term:route` payload.
    /// Verifies the entire wiring: Tauri listener registration, raw
    /// payload deserialization, ANSI strip, marker parse, decision,
    /// emit. Run on `tokio::main` because `tauri::async_runtime::spawn`
    /// (called by `apply_decision` on the happy path) needs a runtime.
    #[tokio::test(flavor = "current_thread")]
    async fn install_routes_a_valid_marker_emits_ok() {
        let app = mock_app_with_terminal_registry();

        let mut panes_by_agent = HashMap::new();
        panes_by_agent.insert("orchestrator".into(), "p-orch".into());
        panes_by_agent.insert("scout".into(), "p-scout".into());

        let captured: Arc<StdMutex<Vec<CapturedRoute>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.handle().listen("swarm-term:route", move |ev| {
            let parsed: CapturedRoute =
                serde_json::from_str(ev.payload()).expect("parse route");
            captured_w.lock().unwrap().push(parsed);
        });

        let _ids = install(app.handle().clone(), panes_by_agent);

        // Emit a synthetic line event mirroring the shape the PTY
        // reader puts on the wire (`{ k, text, seq }`).
        app.emit(
            "panes:p-orch:line",
            serde_json::json!({
                "k": "out",
                "text": ">> @scout: find the db",
                "seq": 1,
            }),
        )
        .expect("emit");

        // Drive the listener channel briefly so the synchronous
        // chain (listen → decide → emit) drains before we assert.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let cap = captured.lock().unwrap();
        assert_eq!(cap.len(), 1, "expected exactly one route emit, got {cap:?}");
        let row = &cap[0];
        assert_eq!(row.source, "orchestrator");
        assert_eq!(row.target, "scout");
        assert_eq!(row.outcome, "ok");
        assert_eq!(row.body, "find the db");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn install_routes_an_unknown_target_emits_unknown() {
        let app = mock_app_with_terminal_registry();

        let mut panes_by_agent = HashMap::new();
        panes_by_agent.insert("orchestrator".into(), "p-orch".into());

        let captured: Arc<StdMutex<Vec<CapturedRoute>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.handle().listen("swarm-term:route", move |ev| {
            let parsed: CapturedRoute =
                serde_json::from_str(ev.payload()).expect("parse");
            captured_w.lock().unwrap().push(parsed);
        });

        let _ids = install(app.handle().clone(), panes_by_agent);

        app.emit(
            "panes:p-orch:line",
            serde_json::json!({
                "k": "out",
                "text": ">> @phantom: hello",
                "seq": 1,
            }),
        )
        .expect("emit");

        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let cap = captured.lock().unwrap();
        assert_eq!(cap.len(), 1);
        assert_eq!(cap[0].outcome, "unknown_target");
        assert_eq!(cap[0].target, "phantom");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn install_emits_denied_for_forbidden_route() {
        let app = mock_app_with_terminal_registry();

        let mut panes_by_agent = HashMap::new();
        panes_by_agent.insert("scout".into(), "p-scout".into());
        // backend-builder pane exists but scout is not permitted to
        // reach it per the hierarchy graph.
        panes_by_agent.insert("backend-builder".into(), "p-bb".into());

        let captured: Arc<StdMutex<Vec<CapturedRoute>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.handle().listen("swarm-term:route", move |ev| {
            let parsed: CapturedRoute =
                serde_json::from_str(ev.payload()).expect("parse");
            captured_w.lock().unwrap().push(parsed);
        });

        let _ids = install(app.handle().clone(), panes_by_agent);

        app.emit(
            "panes:p-scout:line",
            serde_json::json!({
                "k": "out",
                "text": ">> @backend-builder: do thing",
                "seq": 1,
            }),
        )
        .expect("emit");

        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let cap = captured.lock().unwrap();
        assert_eq!(cap.len(), 1);
        assert_eq!(cap[0].outcome, "denied");
        assert_eq!(cap[0].source, "scout");
        assert_eq!(cap[0].target, "backend-builder");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn install_ignores_lines_without_a_marker() {
        let app = mock_app_with_terminal_registry();

        let mut panes_by_agent = HashMap::new();
        panes_by_agent.insert("orchestrator".into(), "p-orch".into());

        let captured: Arc<StdMutex<Vec<CapturedRoute>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let captured_w = Arc::clone(&captured);
        app.handle().listen("swarm-term:route", move |ev| {
            let parsed: CapturedRoute =
                serde_json::from_str(ev.payload()).expect("parse");
            captured_w.lock().unwrap().push(parsed);
        });

        let _ids = install(app.handle().clone(), panes_by_agent);

        app.emit(
            "panes:p-orch:line",
            serde_json::json!({
                "k": "out",
                "text": "just claude saying hello",
                "seq": 1,
            }),
        )
        .expect("emit");

        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let cap = captured.lock().unwrap();
        assert!(cap.is_empty(), "expected zero emits, got {cap:?}");
    }
}

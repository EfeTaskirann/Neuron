//! Task-lifecycle state machine driving the autonomy contract.
//!
//! Per-task state tracking keyed by `(source_pane_id, task_id)`. The
//! bridge watcher's [`crate::swarm_term::bridge::synth::lifecycle_synthesise`]
//! consults this module on every coordinator-bound delivery so the
//! swarm can advance through the goal-cycle without requesting human
//! approval at any intermediate step:
//!
//! 1. [`parse_lifecycle_token`] — pure: classify an envelope body as one
//!    of the four recognised transition tokens (`BUILDING <id>`,
//!    `DONE <id>`, `APPROVED <id>`, `CHANGES_NEEDED <id>`).
//! 2. [`apply_transition`] — pure: derive the new `LifecycleState`
//!    given the prior state + a parsed [`Transition`].
//! 3. [`LifecycleStore::record`] — IO-shell: applies a transition to a
//!    per-session map keyed by `(source_pane, task_id)`.
//! 4. [`followup_for_coordinator_inbound`] — pure: given a coordinator-
//!    bound transition, return the (target_agent, body) of the
//!    synthesised follow-up route, or `None` for transitions that
//!    don't fan out.
//!
//! See [`crate::swarm_term::hierarchy`] for the cycle diagram and the
//! static routing graph that the fanout targets are validated against.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::swarm_term::hierarchy::{
    is_allowed, is_builder, is_reviewer, reviewer_for_builder,
};

/// Body-prefix that signals "the assigned builder started work on
/// the task". Optional in v1 (builders may skip it and go straight
/// to DONE) — recognised so the lifecycle store can display the
/// in-flight state in the UI.
pub const BUILDING_PREFIX: &str = "BUILDING ";

/// Body-prefix that signals "the assigned builder finished the task".
/// Persona docs instruct builders to write `DONE <task_id>` in the
/// envelope body addressed to coordinator once the work is complete.
pub const DONE_PREFIX: &str = "DONE ";

/// Body-prefix that signals "the assigned reviewer approved the
/// task". Persona docs instruct reviewers to write `APPROVED <task_id>`
/// in the envelope body addressed to coordinator after a passing review.
pub const APPROVED_PREFIX: &str = "APPROVED ";

/// Body-prefix that signals "the assigned reviewer rejected the
/// task and is requesting changes from the builder". Optional in
/// v1 — when present, the coordinator persona drives the
/// re-dispatch (rejection comes with specific feedback that we
/// can't synthesise from a static fanout).
pub const CHANGES_NEEDED_PREFIX: &str = "CHANGES_NEEDED ";

/// Body-prefix the coordinator emits to the orchestrator once a
/// reviewer approves a task. Synthesised by
/// [`followup_for_coordinator_inbound`] on the `Approved` transition.
pub const TASK_DONE_PREFIX: &str = "TASK_DONE ";

/// State a single `(source_pane, task_id)` tuple can be in within the
/// autonomy cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// Initial state — coordinator dispatched the task to the builder
    /// but no signal has come back yet. The store auto-inserts an
    /// `Assigned` entry when a non-recognised transition first appears
    /// for an unseen key; explicit `BUILDING` upgrades it.
    Assigned,
    /// Builder acknowledged the task is in-flight via `BUILDING <id>`.
    Building,
    /// Builder fired `DONE <id>`; the lifecycle is waiting for the
    /// reviewer's verdict. The bridge has already synthesised the
    /// follow-up `review <id>` dispatch to the paired reviewer.
    AwaitingReview,
    /// Reviewer fired `APPROVED <id>`; the bridge has synthesised the
    /// follow-up `TASK_DONE <id>` to the orchestrator. The task is
    /// considered closed from the coordinator's perspective.
    Approved,
    /// End state — the orchestrator has acknowledged completion.
    /// Currently set by the same code path as `Approved` because the
    /// orchestrator's acknowledgement is the user-facing one and we
    /// don't synthesise it. Kept distinct for forward-compat: a
    /// future enhancement (e.g. orchestrator emitting `CLOSED <id>`)
    /// can transition `Approved → Done` explicitly.
    Done,
    /// Reviewer fired `CHANGES_NEEDED <id>` and the builder must
    /// re-work the task. Coordinator persona drives the re-dispatch;
    /// the store records the state for UI/diagnostic display.
    Failed,
}

/// The four lifecycle transitions recognised on routed envelope bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransitionKind {
    Building,
    Done,
    Approved,
    ChangesNeeded,
}

/// Parsed lifecycle signal — output of [`parse_lifecycle_token`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub kind: TransitionKind,
    pub task_id: String,
}

/// Hard cap on a lifecycle task id (VAL-01). A single routed body
/// cannot grow the store's key arbitrarily — legitimate ids are short
/// ULIDs / integers, so 128 chars is far above any real id.
const MAX_TASK_ID_LEN: usize = 128;

/// Soft cap on distinct tracked lifecycle entries (VAL-01). When
/// exceeded, `record` evicts terminal-state entries (Approved / Failed
/// / Done) — they only drive a brief UI badge — to bound memory over a
/// long session that sees many unique task ids.
const MAX_LIFECYCLE_ENTRIES: usize = 2048;

/// Cap on the free-text note carried from a builder's `DONE` /
/// reviewer's `APPROVED` body into the synthesised follow-up, so the
/// reviewer learns WHAT changed without one body flooding a pane.
const MAX_FOLLOWUP_NOTE_CHARS: usize = 1500;

/// Keyword spellings, matched case-insensitively on the lenient path.
/// The canonical form of each kind is its `*_PREFIX` constant (see
/// [`canonical_prefix`]).
const KEYWORDS: &[(&str, TransitionKind)] = &[
    ("BUILDING", TransitionKind::Building),
    ("DONE", TransitionKind::Done),
    ("APPROVED", TransitionKind::Approved),
    ("CHANGES_NEEDED", TransitionKind::ChangesNeeded),
    ("CHANGES-NEEDED", TransitionKind::ChangesNeeded),
    ("CHANGES NEEDED", TransitionKind::ChangesNeeded),
];

/// Leading decoration agents wrap a token in (`**DONE** T4`,
/// `` `DONE` T4 ``, `> DONE T4`), stripped before keyword matching.
fn is_decoration(c: char) -> bool {
    c.is_whitespace() || matches!(c, '*' | '_' | '`' | '#' | '>' | '~')
}

/// Separators between the keyword and the id (`DONE: T3`,
/// `DONE — T3`, the closing `**` of `**DONE**`).
fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(c, ':' | '-' | '—' | '–' | '=' | '*' | '_' | '`' | '~')
}

/// Punctuation trimmed off both ends of an id token (`T5.`, `` `T5` ``).
fn is_id_trim(c: char) -> bool {
    matches!(
        c,
        '*' | '_' | '`' | ',' | '.' | ':' | ';' | '(' | ')' | '[' | ']' | '"' | '\''
    )
}

fn has_digit(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
}

/// A body's leading lifecycle keyword. `canonical` is true for the
/// exact persona-taught form (`DONE <id>`: upper-case, undecorated,
/// one space) — the only form whose id may be digit-free.
struct Head<'a> {
    kind: TransitionKind,
    canonical: bool,
    rest: &'a str,
}

fn split_head(body: &str) -> Option<Head<'_>> {
    let trimmed = body.trim();
    let head = trimmed.trim_start_matches(is_decoration);
    for &(kw, kind) in KEYWORDS {
        let Some(cand) = head.get(..kw.len()) else {
            continue;
        };
        if !cand.eq_ignore_ascii_case(kw) {
            continue;
        }
        let rest = &head[kw.len()..];
        // Word boundary: `DONEX` / `Donation` are not the keyword.
        if rest.chars().next().is_some_and(char::is_alphanumeric) {
            continue;
        }
        let canonical = trimmed.starts_with(canonical_prefix(kind));
        return Some(Head { kind, canonical, rest });
    }
    None
}

fn canonical_prefix(kind: TransitionKind) -> &'static str {
    match kind {
        TransitionKind::Building => BUILDING_PREFIX,
        TransitionKind::Done => DONE_PREFIX,
        TransitionKind::Approved => APPROVED_PREFIX,
        TransitionKind::ChangesNeeded => CHANGES_NEEDED_PREFIX,
    }
}

fn inline_id(rest: &str) -> Option<&str> {
    let tok = rest
        .trim_start_matches(is_separator)
        .split_whitespace()
        .next()?
        .trim_matches(is_id_trim);
    (!tok.is_empty() && tok.len() <= MAX_TASK_ID_LEN).then_some(tok)
}

/// True when the text right after the keyword reads as "end of signal"
/// (`DONE`, `DONE — note`, `**DONE**.`) rather than prose that merely
/// starts with the word (`Done reading the file`).
fn ends_signal(rest: &str) -> bool {
    let r = rest.trim_start();
    r.is_empty()
        || r.starts_with(|c: char| {
            matches!(c, ':' | '-' | '—' | '–' | '=' | '*' | '`' | '_' | '~' | '.' | '!')
        })
}

/// Pure: extract a [`Transition`] from a routed envelope body. Returns
/// `None` if the body doesn't start with one of the four lifecycle
/// keywords or no usable task id follows it.
///
/// Matching is lenient about the shapes LLM senders actually produce:
/// case (`Done T5`), leading markdown (`**DONE** T4`), and separators
/// (`DONE: T3`, `DONE — T3`). To keep prose like `done so far` from
/// firing a review, a NON-canonical match only counts when its id
/// contains a digit (every persona-taught id does: `T1`, `fb-7`, `42`).
/// Anything after the task id (extra notes) is ignored here — see
/// [`lifecycle_note`]. Equivalent to
/// [`parse_lifecycle_token_with_fallback`] without an envelope id.
pub fn parse_lifecycle_token(body: &str) -> Option<Transition> {
    parse_lifecycle_token_with_fallback(body, None)
}

/// Like [`parse_lifecycle_token`] but also consults the structured
/// `Envelope.task_id` field:
///
///   * bare / punctuated keyword (`"DONE"`, `"DONE — implemented X"`)
///     with no usable inline id → the envelope's `task_id` is the id;
///   * an inline "id" that is a plain word (`"DONE implemented X"`) is
///     overridden by the envelope's `task_id`;
///   * a real inline id (`"DONE 99"`) wins over the field.
///
/// The fallback exists because the persona footer teaches agents to
/// populate BOTH the body token and the envelope's `task_id`; without
/// honouring `task_id`, an agent that filled only the structured field
/// would silently fail to fan out.
pub fn parse_lifecycle_token_with_fallback(
    body: &str,
    fallback_task_id: Option<&str>,
) -> Option<Transition> {
    let fallback = fallback_task_id
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_TASK_ID_LEN);
    let head = split_head(body)?;
    let inline = inline_id(head.rest)
        .filter(|id| head.canonical || has_digit(id) || Some(*id) == fallback);
    let task_id = match (inline, fallback) {
        (Some(id), Some(fb)) if !has_digit(id) => fb,
        (Some(id), _) => id,
        (None, Some(fb)) if ends_signal(head.rest) => fb,
        _ => return None,
    };
    Some(Transition {
        kind: head.kind,
        task_id: task_id.to_string(),
    })
}

/// Pure: the free-text note following the keyword + id of a lifecycle
/// body (`"DONE T1 — auth.rs: token refresh eklendi"` →
/// `"auth.rs: token refresh eklendi"`), or `None` when there is none.
/// Carried into the synthesised follow-up so the reviewer knows what to
/// look at instead of receiving a bare `review T1`.
pub fn lifecycle_note(body: &str, task_id: &str) -> Option<String> {
    let head = split_head(body)?;
    let mut rest = head.rest.trim_start_matches(is_separator);
    if let Some(first) = rest.split_whitespace().next() {
        if first.trim_matches(is_id_trim) == task_id {
            rest = &rest[first.len()..];
        }
    }
    let note = rest
        .trim_start_matches(|c| is_separator(c) || matches!(c, '.' | ',' | ';'))
        .trim();
    if note.is_empty() {
        return None;
    }
    let capped = crate::text::truncate_chars(note, MAX_FOLLOWUP_NOTE_CHARS);
    Some(if capped.len() < note.len() {
        format!("{capped}…")
    } else {
        capped
    })
}

/// Pure: true when a body is the 4-state contract's failure report
/// (`hata — <sebep>`, any case / leading markdown). Paired with the
/// envelope's `task_id` it closes that task (see
/// [`LifecycleStore::abandon`]).
pub fn is_error_report(body: &str) -> bool {
    let head = body.trim().trim_start_matches(is_decoration);
    head.get(..4).is_some_and(|w| w.eq_ignore_ascii_case("hata"))
        && !head[4..].chars().next().is_some_and(char::is_alphanumeric)
}

/// Pure: true when a coordinator-bound body LOOKS like a lifecycle
/// signal (starts with `done` / `approved` / `changes needed` / …, or —
/// when the envelope carries a `task_id` — the Turkish completion word
/// `tamam`) but [`parse_lifecycle_token_with_fallback`] rejected it.
/// The bridge uses this to tell the sender its signal was NOT acted
/// on, instead of the chain silently stalling.
pub fn looks_like_unparsed_signal(body: &str, has_task_id: bool) -> bool {
    let head: String = body
        .trim()
        .trim_start_matches(is_decoration)
        .chars()
        .take(24)
        .collect::<String>()
        .to_lowercase();
    // Every recognised keyword spelling (from KEYWORDS — the single
    // source), plus near-misses that are not keywords themselves.
    const EXTRA_HINTS: &[&str] = &["approve", "lgtm"];
    KEYWORDS
        .iter()
        .any(|(kw, _)| head.starts_with(&kw.to_ascii_lowercase()))
        || EXTRA_HINTS.iter().any(|h| head.starts_with(h))
        || (has_task_id && (head.starts_with("tamam") || head.starts_with("onay")))
}

/// Pure: derive the new lifecycle state after applying `transition`.
///
/// `prev_state` is `None` when this is the first signal seen for a
/// given `(source_pane, task_id)` key. The state machine is
/// permissive — each transition unambiguously determines the new
/// state regardless of the prior state — so the function returns the
/// new state without rejecting any transition. The signature still
/// carries `prev_state` to leave the door open for stricter
/// sequencing in v2 (e.g. `DONE` only valid from `Building`).
///
/// State transitions:
///
/// ```text
///   *  ──BUILDING────────▶ Building
///   *  ──DONE────────────▶ AwaitingReview
///   *  ──APPROVED────────▶ Approved
///   *  ──CHANGES_NEEDED──▶ Failed
/// ```
pub fn apply_transition(
    _prev_state: Option<LifecycleState>,
    transition: &Transition,
) -> LifecycleState {
    match transition.kind {
        TransitionKind::Building => LifecycleState::Building,
        TransitionKind::Done => LifecycleState::AwaitingReview,
        TransitionKind::Approved => LifecycleState::Approved,
        TransitionKind::ChangesNeeded => LifecycleState::Failed,
    }
}

/// Per-session lifecycle store. Keyed by
/// `(source_pane_id, task_id)` so a single agent can have several
/// in-flight tasks concurrently (one entry per task) without state
/// collisions.
///
/// The store is owned by the session and read by the bridge watcher
/// (`bridge::synth::lifecycle_synthesise`) on each coordinator-bound delivery.
/// Reads and writes are guarded by a single `Mutex` — the call rate is
/// bounded by the route emit rate (~1 Hz worst case), so contention is
/// not a concern.
///
/// Each entry also remembers WHEN it entered its state and how many
/// stall reminders it has produced, so [`LifecycleStore::take_stalls`]
/// can flag a task stuck in `Building` / `AwaitingReview`.
#[derive(Default)]
pub struct LifecycleStore {
    inner: Mutex<HashMap<(String, String), Entry>>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    state: LifecycleState,
    since: Instant,
    /// Highest stall level already reported for this stay in `state`.
    reminders: u8,
}

impl Entry {
    fn new(state: LifecycleState, now: Instant) -> Self {
        Self { state, since: now, reminders: 0 }
    }
}

/// One in-flight task that crossed a stall threshold — output of
/// [`LifecycleStore::take_stalls`]. `level` is 1 at the threshold and 2
/// at twice the threshold (escalation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub source_pane: String,
    pub task_id: String,
    pub state: LifecycleState,
    pub age: Duration,
    pub level: u8,
}

impl LifecycleStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a transition to the store and return the new state.
    /// Inserts the entry if it does not exist.
    pub fn record(
        &self,
        source_pane: &str,
        transition: &Transition,
    ) -> LifecycleState {
        self.record_at(source_pane, transition, Instant::now())
    }

    fn record_at(
        &self,
        source_pane: &str,
        transition: &Transition,
        now: Instant,
    ) -> LifecycleState {
        let key = (source_pane.to_string(), transition.task_id.clone());
        // CONC-01: recover the guard on poison instead of panicking on
        // this hot path. A poisoned lock only means some *other* holder
        // panicked; the HashMap itself is intact, and this store is
        // display-only (not load-bearing for message routing), so it is
        // always safe to keep using it. `state_of`/`len`/`mark_done`
        // already degrade gracefully — `record` was the lone panic.
        let mut g = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // VAL-01: bound memory over a long session. If we're at the cap
        // and about to add a new key, evict terminal-state entries first
        // (Approved / Failed / Done are closed; they only linger for a
        // brief badge), keeping in-flight (Building / AwaitingReview).
        if g.len() >= MAX_LIFECYCLE_ENTRIES && !g.contains_key(&key) {
            g.retain(|_, e| {
                !matches!(
                    e.state,
                    LifecycleState::Approved
                        | LifecycleState::Failed
                        | LifecycleState::Done
                )
            });
        }
        let prev = g.get(&key).map(|e| e.state);
        let next = apply_transition(prev, transition);
        g.insert(key.clone(), Entry::new(next, now));
        // A verdict is recorded under the REVIEWER's pane, but the task
        // it judges sits under the builder's pane in `AwaitingReview`.
        // Close that entry too — otherwise every reviewed task would
        // look stalled forever.
        let builder_next = match transition.kind {
            TransitionKind::Approved => Some(LifecycleState::Done),
            TransitionKind::ChangesNeeded => Some(LifecycleState::Failed),
            TransitionKind::Building | TransitionKind::Done => None,
        };
        if let Some(builder_next) = builder_next {
            for (k, e) in g.iter_mut() {
                if k != &key
                    && k.1 == transition.task_id
                    && e.state == LifecycleState::AwaitingReview
                {
                    *e = Entry::new(builder_next, now);
                }
            }
        }
        next
    }

    /// The responsible agent gave up on `task_id` (`hata — …` with the
    /// envelope's `task_id`): mark the in-flight (`Building` /
    /// `AwaitingReview`) entries for that task as `Failed`, so the stall
    /// timer stops reminding / escalating a task already reported as
    /// failed. Only entries under `closable_panes` are touched — the
    /// reporter's own pane, plus (for a reviewer) the panes of the
    /// builders it reviews — so an unrelated agent's `hata` about the
    /// same id cannot end someone else's pending review. Never creates
    /// entries. Returns whether anything closed.
    pub fn abandon(&self, closable_panes: &[&str], task_id: &str) -> bool {
        self.abandon_at(closable_panes, task_id, Instant::now())
    }

    fn abandon_at(&self, closable_panes: &[&str], task_id: &str, now: Instant) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        let mut closed = false;
        for ((pane, id), e) in g.iter_mut() {
            let in_flight = matches!(
                e.state,
                LifecycleState::Building | LifecycleState::AwaitingReview
            );
            if id == task_id && in_flight && closable_panes.contains(&pane.as_str()) {
                *e = Entry::new(LifecycleState::Failed, now);
                closed = true;
            }
        }
        closed
    }

    /// Report in-flight tasks that crossed a stall threshold since the
    /// last call: `AwaitingReview` older than `review_after`, `Building`
    /// older than `build_after`. Each entry reports level 1 once at the
    /// threshold and level 2 once at twice the threshold; a new
    /// transition resets the clock.
    pub fn take_stalls(
        &self,
        now: Instant,
        review_after: Duration,
        build_after: Duration,
    ) -> Vec<Stall> {
        let Ok(mut g) = self.inner.lock() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for ((pane, task_id), e) in g.iter_mut() {
            let threshold = match e.state {
                LifecycleState::AwaitingReview => review_after,
                LifecycleState::Building => build_after,
                _ => continue,
            };
            let age = now.saturating_duration_since(e.since);
            let level: u8 = if age >= threshold * 2 {
                2
            } else if age >= threshold {
                1
            } else {
                0
            };
            if level > e.reminders {
                e.reminders = level;
                out.push(Stall {
                    source_pane: pane.clone(),
                    task_id: task_id.clone(),
                    state: e.state,
                    age,
                    level,
                });
            }
        }
        out.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        out
    }

    /// Look up the current state without mutating. Returns `None`
    /// for an unseen `(source_pane, task_id)` pair.
    ///
    /// Test seam: production consumers react to the
    /// `swarm-term:lifecycle` event stream instead of polling the
    /// store, so this accessor is gated to tests — ungate it if a
    /// poll-style UI consumer ever appears.
    #[cfg(test)]
    pub fn state_of(
        &self,
        source_pane: &str,
        task_id: &str,
    ) -> Option<LifecycleState> {
        let g = self.inner.lock().ok()?;
        g.get(&(source_pane.to_string(), task_id.to_string()))
            .map(|e| e.state)
    }

    /// Number of distinct `(source_pane, task_id)` entries currently
    /// tracked. Exposed for diagnostic / smoke-test use.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// True when no transitions have been recorded yet. Companion to
    /// [`len`] — kept so clippy's `len_without_is_empty` lint is
    /// satisfied and so UI consumers can decide whether to render the
    /// "no in-flight tasks" empty state without a `len() == 0` check.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Mark `task_id` as `Done` regardless of prior state. Used by
    /// the bridge after the synthesised `TASK_DONE` fanout to the
    /// orchestrator has been emitted — the cycle is closed from
    /// the coordinator's perspective and the store should reflect
    /// the terminal state without waiting for a (currently
    /// unimplemented) explicit `CLOSED` signal.
    pub fn mark_done(&self, source_pane: &str, task_id: &str) {
        let key = (source_pane.to_string(), task_id.to_string());
        if let Ok(mut g) = self.inner.lock() {
            g.insert(key, Entry::new(LifecycleState::Done, Instant::now()));
        }
    }
}

/// Pure: given a coordinator-bound transition fired by `src_agent`,
/// return the `(target_agent, body)` of the synthesised follow-up
/// route — or `None` if the transition does not warrant fanout.
///
/// Fanouts:
///
///   * `Done` from a builder → `(<paired reviewer>, "review <id>")`
///   * `Approved` from a reviewer → `("orchestrator",
///                                    "TASK_DONE <id>")`
///
/// `Building` / `ChangesNeeded` do NOT fan out: the coordinator pane
/// has already recorded the signal via the original route, and any
/// downstream action (rework dispatch with concrete feedback) is
/// driven by the coordinator persona — not synthesisable from the
/// static prefix alone.
///
/// Hierarchy gate: this helper short-circuits to `None` for any
/// fanout whose `(coordinator → target)` edge is not present in
/// `hierarchy::ALLOWED`. The bridge's canonical
/// `is_allowed(coordinator, target)` check still runs at delivery
/// time as a second line of defence.
pub fn followup_for_coordinator_inbound(
    src_agent: &str,
    transition: &Transition,
) -> Option<(String, String)> {
    match transition.kind {
        TransitionKind::Done => {
            // Only builders can fire a DONE that warrants synthesis —
            // anyone else writing `DONE <id>` to coordinator is prose,
            // not a lifecycle signal.
            if !is_builder(src_agent) {
                return None;
            }
            let reviewer = reviewer_for_builder(src_agent)?;
            if !is_allowed("coordinator", reviewer) {
                return None;
            }
            Some((
                reviewer.to_string(),
                format!("review {}", transition.task_id),
            ))
        }
        TransitionKind::Approved => {
            if !is_reviewer(src_agent) {
                return None;
            }
            if !is_allowed("coordinator", "orchestrator") {
                return None;
            }
            Some((
                "orchestrator".to_string(),
                format!("{TASK_DONE_PREFIX}{}", transition.task_id),
            ))
        }
        // BUILDING — purely informational; the coordinator pane already
        // received the message, the UI lifecycle pill updates from the
        // store. No fanout.
        // CHANGES_NEEDED — rejection comes with concrete reviewer
        // feedback that the coordinator persona uses to compose a
        // targeted re-dispatch. Synthesising a generic "rework <id>"
        // would drop the feedback and devolve into a builder loop.
        TransitionKind::Building | TransitionKind::ChangesNeeded => None,
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_lifecycle_token --------------------------------------- //

    #[test]
    fn parses_done_token() {
        let t = parse_lifecycle_token("DONE 42").unwrap();
        assert_eq!(t.kind, TransitionKind::Done);
        assert_eq!(t.task_id, "42");
    }

    #[test]
    fn parses_approved_token() {
        let t = parse_lifecycle_token("APPROVED 99").unwrap();
        assert_eq!(t.kind, TransitionKind::Approved);
        assert_eq!(t.task_id, "99");
    }

    #[test]
    fn parses_building_token() {
        let t = parse_lifecycle_token("BUILDING feat-7").unwrap();
        assert_eq!(t.kind, TransitionKind::Building);
        assert_eq!(t.task_id, "feat-7");
    }

    #[test]
    fn parses_changes_needed_token() {
        let t = parse_lifecycle_token("CHANGES_NEEDED 42").unwrap();
        assert_eq!(t.kind, TransitionKind::ChangesNeeded);
        assert_eq!(t.task_id, "42");
    }

    #[test]
    fn drops_extra_notes_after_task_id() {
        // Builders sometimes append context after the id; the parser
        // keeps only the id so the synthesised follow-up route stays
        // short and stable.
        let t = parse_lifecycle_token("DONE 42 — also fixed lint").unwrap();
        assert_eq!(t.task_id, "42");
    }

    #[test]
    fn tolerates_leading_whitespace_from_body_assembler() {
        // A wrapped envelope body can arrive with leading whitespace
        // (e.g. a body that starts on the second physical line).
        let t = parse_lifecycle_token("\n  DONE 42").unwrap();
        assert_eq!(t.kind, TransitionKind::Done);
        assert_eq!(t.task_id, "42");
    }

    #[test]
    fn rejects_empty_task_id() {
        assert!(parse_lifecycle_token("DONE ").is_none());
        assert!(parse_lifecycle_token("DONE     ").is_none());
        assert!(parse_lifecycle_token("APPROVED").is_none());
    }

    // -- parse_lifecycle_token_with_fallback ------------------------- //

    #[test]
    fn fallback_uses_envelope_task_id_for_bare_keyword() {
        // Body is a bare keyword with no inline id; the structured
        // envelope.task_id supplies it.
        let t = parse_lifecycle_token_with_fallback("DONE", Some("42")).unwrap();
        assert_eq!(t.kind, TransitionKind::Done);
        assert_eq!(t.task_id, "42");
        let t = parse_lifecycle_token_with_fallback("APPROVED", Some("fb-7"))
            .unwrap();
        assert_eq!(t.kind, TransitionKind::Approved);
        assert_eq!(t.task_id, "fb-7");
    }

    #[test]
    fn fallback_prefers_inline_body_id_over_envelope_field() {
        // When the body already carries the id, the fallback must not
        // override it with the envelope's task_id.
        let t = parse_lifecycle_token_with_fallback("DONE 99", Some("42"))
            .unwrap();
        assert_eq!(t.task_id, "99");
    }

    #[test]
    fn fallback_none_without_id_anywhere() {
        assert!(parse_lifecycle_token_with_fallback("DONE", None).is_none());
        assert!(parse_lifecycle_token_with_fallback("DONE", Some("  ")).is_none());
        // Non-lifecycle body + a task_id must still not synthesise.
        assert!(
            parse_lifecycle_token_with_fallback("just a note", Some("42"))
                .is_none()
        );
    }

    #[test]
    fn rejects_non_lifecycle_bodies() {
        for body in [
            "",
            "regular dispatch message",
            "yapıyorum",
            "done so far",
            "review pending",
            "approval expected",
            "almost DONE 42", // prefix not at start (post-trim)
            "Done reading the file",
            "Donation 42",
            "**done** with step one",
        ] {
            assert!(
                parse_lifecycle_token(body).is_none(),
                "body `{body}` must not parse as a lifecycle token"
            );
        }
    }

    #[test]
    fn parses_decorated_and_mixed_case_tokens() {
        // The exact shapes that silently failed in field testing (T3).
        for (body, kind, id) in [
            ("DONE: T3", TransitionKind::Done, "T3"),
            ("**DONE** T4", TransitionKind::Done, "T4"),
            ("Done T5", TransitionKind::Done, "T5"),
            ("done — T6 auth.rs bitti", TransitionKind::Done, "T6"),
            ("`APPROVED` fb-7.", TransitionKind::Approved, "fb-7"),
            ("Changes needed: T8 — fix x", TransitionKind::ChangesNeeded, "T8"),
            ("> BUILDING T9", TransitionKind::Building, "T9"),
        ] {
            let t = parse_lifecycle_token(body)
                .unwrap_or_else(|| panic!("`{body}` must parse"));
            assert_eq!(t.kind, kind, "kind for `{body}`");
            assert_eq!(t.task_id, id, "id for `{body}`");
        }
    }

    #[test]
    fn canonical_form_still_accepts_digit_free_ids() {
        let t = parse_lifecycle_token("DONE feat").unwrap();
        assert_eq!(t.task_id, "feat");
        // ...but the lenient form does not (prose guard).
        assert!(parse_lifecycle_token("Done feat").is_none());
    }

    #[test]
    fn fallback_uses_envelope_id_for_punctuated_bare_keyword() {
        let t = parse_lifecycle_token_with_fallback(
            "**DONE** — auth.rs refresh eklendi",
            Some("T2"),
        )
        .unwrap();
        assert_eq!(t.kind, TransitionKind::Done);
        assert_eq!(t.task_id, "T2");
        // A plain word in the id slot yields to the structured field.
        let t = parse_lifecycle_token_with_fallback(
            "DONE implemented the endpoint",
            Some("T2"),
        )
        .unwrap();
        assert_eq!(t.task_id, "T2");
        // Prose that merely starts with the word does not fire.
        assert!(
            parse_lifecycle_token_with_fallback("Done reading, next up", Some("T2"))
                .is_none()
        );
    }

    // -- lifecycle_note / looks_like_unparsed_signal ----------------- //

    #[test]
    fn lifecycle_note_extracts_text_after_id() {
        assert_eq!(
            lifecycle_note("DONE T1 — auth.rs: refresh eklendi", "T1").as_deref(),
            Some("auth.rs: refresh eklendi")
        );
        assert_eq!(
            lifecycle_note("**DONE** — src/x.rs", "T1").as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(lifecycle_note("DONE T1", "T1"), None);
        assert_eq!(lifecycle_note("DONE T1 —  ", "T1"), None);
    }

    #[test]
    fn lifecycle_note_is_capped() {
        let body = format!("DONE T1 — {}", "x".repeat(5000));
        let note = lifecycle_note(&body, "T1").unwrap();
        assert!(note.chars().count() <= MAX_FOLLOWUP_NOTE_CHARS + 1);
    }

    #[test]
    fn unparsed_signal_detection() {
        assert!(looks_like_unparsed_signal("DONE", false));
        assert!(looks_like_unparsed_signal("**Done** with it", false));
        assert!(looks_like_unparsed_signal("approve edildi", false));
        assert!(looks_like_unparsed_signal("tamam — bitti", true));
        // `tamam` without a task context is an ordinary ack.
        assert!(!looks_like_unparsed_signal("tamam — bitti", false));
        assert!(!looks_like_unparsed_signal("alındı — başlıyorum", true));
    }

    // -- apply_transition -------------------------------------------- //

    #[test]
    fn apply_transition_done_yields_awaiting_review() {
        let t = Transition {
            kind: TransitionKind::Done,
            task_id: "42".into(),
        };
        assert_eq!(apply_transition(None, &t), LifecycleState::AwaitingReview);
        assert_eq!(
            apply_transition(Some(LifecycleState::Building), &t),
            LifecycleState::AwaitingReview
        );
    }

    #[test]
    fn apply_transition_approved_yields_approved() {
        let t = Transition {
            kind: TransitionKind::Approved,
            task_id: "42".into(),
        };
        assert_eq!(apply_transition(None, &t), LifecycleState::Approved);
        assert_eq!(
            apply_transition(Some(LifecycleState::AwaitingReview), &t),
            LifecycleState::Approved
        );
    }

    #[test]
    fn apply_transition_changes_needed_yields_failed() {
        let t = Transition {
            kind: TransitionKind::ChangesNeeded,
            task_id: "42".into(),
        };
        assert_eq!(apply_transition(None, &t), LifecycleState::Failed);
    }

    #[test]
    fn apply_transition_building_yields_building() {
        let t = Transition {
            kind: TransitionKind::Building,
            task_id: "42".into(),
        };
        assert_eq!(apply_transition(None, &t), LifecycleState::Building);
    }

    // -- LifecycleStore ---------------------------------------------- //

    #[test]
    fn store_records_independent_tasks_per_source() {
        let s = LifecycleStore::new();
        let bd = Transition {
            kind: TransitionKind::Building,
            task_id: "1".into(),
        };
        let dn = Transition {
            kind: TransitionKind::Done,
            task_id: "2".into(),
        };
        s.record("p-backend-builder", &bd);
        s.record("p-frontend-builder", &dn);
        assert_eq!(
            s.state_of("p-backend-builder", "1"),
            Some(LifecycleState::Building)
        );
        assert_eq!(
            s.state_of("p-frontend-builder", "2"),
            Some(LifecycleState::AwaitingReview)
        );
        // Per-source isolation: the same task_id on a different source
        // pane is a different entry.
        assert_eq!(s.state_of("p-backend-builder", "2"), None);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn store_overwrites_prior_state_on_new_transition() {
        let s = LifecycleStore::new();
        let bd = Transition {
            kind: TransitionKind::Building,
            task_id: "42".into(),
        };
        let dn = Transition {
            kind: TransitionKind::Done,
            task_id: "42".into(),
        };
        assert_eq!(
            s.record("p-backend-builder", &bd),
            LifecycleState::Building
        );
        assert_eq!(
            s.record("p-backend-builder", &dn),
            LifecycleState::AwaitingReview
        );
    }

    #[test]
    fn store_mark_done_terminalises_state() {
        let s = LifecycleStore::new();
        let dn = Transition {
            kind: TransitionKind::Done,
            task_id: "42".into(),
        };
        s.record("p-backend-builder", &dn);
        s.mark_done("p-backend-builder", "42");
        assert_eq!(
            s.state_of("p-backend-builder", "42"),
            Some(LifecycleState::Done)
        );
    }

    // -- take_stalls ------------------------------------------------- //

    const MIN: Duration = Duration::from_secs(60);

    fn tr(kind: TransitionKind, id: &str) -> Transition {
        Transition { kind, task_id: id.into() }
    }

    #[test]
    fn stalls_fire_once_per_level_and_reset_on_transition() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.record_at("p-builder", &tr(TransitionKind::Done, "T1"), t0);

        assert!(s.take_stalls(t0 + 9 * MIN, 10 * MIN, 20 * MIN).is_empty());
        let l1 = s.take_stalls(t0 + 10 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(l1.len(), 1);
        assert_eq!(l1[0].level, 1);
        assert_eq!(l1[0].state, LifecycleState::AwaitingReview);
        // Same level is not reported twice.
        assert!(s.take_stalls(t0 + 15 * MIN, 10 * MIN, 20 * MIN).is_empty());
        let l2 = s.take_stalls(t0 + 20 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(l2[0].level, 2);
        assert!(s.take_stalls(t0 + 60 * MIN, 10 * MIN, 20 * MIN).is_empty());

        // A new transition restarts the clock.
        s.record_at("p-builder", &tr(TransitionKind::Done, "T1"), t0 + 61 * MIN);
        assert!(s.take_stalls(t0 + 65 * MIN, 10 * MIN, 20 * MIN).is_empty());
    }

    #[test]
    fn building_uses_its_own_threshold_and_terminal_states_never_stall() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.record_at("p-b", &tr(TransitionKind::Building, "T2"), t0);
        s.record_at("p-r", &tr(TransitionKind::Approved, "T3"), t0);
        assert!(s.take_stalls(t0 + 15 * MIN, 10 * MIN, 20 * MIN).is_empty());
        let st = s.take_stalls(t0 + 20 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].task_id, "T2");
        assert_eq!(st[0].state, LifecycleState::Building);
    }

    #[test]
    fn error_report_closes_the_stalled_task_and_stops_reminders() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.record_at("p-fb", &tr(TransitionKind::Building, "stall-1"), t0);
        // Builder reports `hata —` for its own task.
        assert!(s.abandon_at(&["p-fb"], "stall-1", t0 + MIN));
        assert_eq!(s.state_of("p-fb", "stall-1"), Some(LifecycleState::Failed));
        assert!(s.take_stalls(t0 + 90 * MIN, 10 * MIN, 20 * MIN).is_empty());

        // The paired reviewer's `hata —` settles the builder's
        // AwaitingReview entry (the caller passes the builder's pane).
        s.record_at("p-bb", &tr(TransitionKind::Done, "T8"), t0);
        assert!(s.abandon_at(&["p-br", "p-bb"], "T8", t0));
        assert_eq!(s.state_of("p-bb", "T8"), Some(LifecycleState::Failed));

        // An unrelated agent (e.g. scout) reporting `hata` about the same
        // id can close neither a review wait nor another pane's Building.
        s.record_at("p-bb", &tr(TransitionKind::Done, "T10"), t0);
        assert!(!s.abandon_at(&["p-scout"], "T10", t0));
        assert_eq!(s.state_of("p-bb", "T10"), Some(LifecycleState::AwaitingReview));
        s.record_at("p-x", &tr(TransitionKind::Building, "T9"), t0);
        assert!(!s.abandon_at(&["p-y"], "T9", t0));
        assert_eq!(s.state_of("p-x", "T9"), Some(LifecycleState::Building));
        // Unknown ids create nothing.
        assert!(!s.abandon_at(&["p-fb"], "nope", t0));
        assert_eq!(s.state_of("p-fb", "nope"), None);
    }

    #[test]
    fn error_report_detection() {
        for body in ["hata — yapamadım", "Hata: dosya yok", "**HATA** — x", "hata"] {
            assert!(is_error_report(body), "{body}");
        }
        for body in ["hatalı satır düzeltildi", "tamam — hata yok", "", "hat"] {
            assert!(!is_error_report(body), "{body}");
        }
    }

    #[test]
    fn reviewer_verdict_closes_the_builders_awaiting_entry() {
        // DONE is keyed by the builder pane, APPROVED by the reviewer
        // pane — the verdict must still settle the builder's entry, or
        // every reviewed task would be reported stalled.
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.record_at("p-builder", &tr(TransitionKind::Done, "T4"), t0);
        s.record_at("p-reviewer", &tr(TransitionKind::Approved, "T4"), t0 + MIN);
        assert_eq!(s.state_of("p-builder", "T4"), Some(LifecycleState::Done));
        assert!(s.take_stalls(t0 + 90 * MIN, 10 * MIN, 20 * MIN).is_empty());

        s.record_at("p-builder", &tr(TransitionKind::Done, "T5"), t0);
        s.record_at("p-reviewer", &tr(TransitionKind::ChangesNeeded, "T5"), t0);
        assert_eq!(s.state_of("p-builder", "T5"), Some(LifecycleState::Failed));
        // Other tasks are untouched.
        s.record_at("p-builder", &tr(TransitionKind::Done, "T6"), t0);
        s.record_at("p-reviewer", &tr(TransitionKind::Approved, "T7"), t0);
        assert_eq!(
            s.state_of("p-builder", "T6"),
            Some(LifecycleState::AwaitingReview)
        );
    }

    // -- followup_for_coordinator_inbound ---------------------------- //

    #[test]
    fn followup_done_from_backend_builder_pairs_with_backend_reviewer() {
        let t = Transition {
            kind: TransitionKind::Done,
            task_id: "42".into(),
        };
        let (target, body) = followup_for_coordinator_inbound(
            "backend-builder",
            &t,
        )
        .unwrap();
        assert_eq!(target, "backend-reviewer");
        assert_eq!(body, "review 42");
    }

    #[test]
    fn followup_done_from_frontend_builder_pairs_with_frontend_reviewer() {
        let t = Transition {
            kind: TransitionKind::Done,
            task_id: "feat-7".into(),
        };
        let (target, body) = followup_for_coordinator_inbound(
            "frontend-builder",
            &t,
        )
        .unwrap();
        assert_eq!(target, "frontend-reviewer");
        assert_eq!(body, "review feat-7");
    }

    #[test]
    fn followup_approved_from_reviewer_routes_to_orchestrator() {
        // All three reviewer roles must close the cycle. Adding
        // integration-tester here pins the symmetry with the
        // frontend's REVIEWER_AGENTS set + hierarchy::is_reviewer:
        // an APPROVED from the final-stage tester is a valid signal
        // that the entire change set passes end-to-end, and the
        // cycle MUST close on it.
        let t = Transition {
            kind: TransitionKind::Approved,
            task_id: "99".into(),
        };
        for reviewer in [
            "backend-reviewer",
            "frontend-reviewer",
            "integration-tester",
        ] {
            let (target, body) =
                followup_for_coordinator_inbound(reviewer, &t).unwrap();
            assert_eq!(
                target, "orchestrator",
                "{reviewer} APPROVED must fan out to orchestrator"
            );
            assert_eq!(
                body, "TASK_DONE 99",
                "{reviewer} APPROVED fanout body must carry TASK_DONE"
            );
        }
    }

    #[test]
    fn followup_approved_from_integration_tester_closes_cycle() {
        // Regression pin for Coordinator's MED#1 polish ask
        // (2026-05-15): integration-tester's APPROVED used to fall
        // into the "non-reviewer" branch because the old is_reviewer
        // matched only the two domain reviewers. The UI hook
        // (REVIEWER_AGENTS) already treated it as a reviewer — without
        // this fix the frontend lifecycle pill and the backend
        // fanout disagreed on its role.
        let t = Transition {
            kind: TransitionKind::Approved,
            task_id: "smoke-42".into(),
        };
        let (target, body) =
            followup_for_coordinator_inbound("integration-tester", &t)
                .expect(
                    "integration-tester APPROVED must trigger TASK_DONE \
                     fanout (Coordinator MED#1 contract)",
                );
        assert_eq!(target, "orchestrator");
        assert_eq!(body, "TASK_DONE smoke-42");
    }

    #[test]
    fn followup_skips_done_from_non_builder() {
        // scout / planner / integration-tester / reviewers /
        // orchestrator MUST NOT trigger a review-dispatch.
        let t = Transition {
            kind: TransitionKind::Done,
            task_id: "42".into(),
        };
        for non_builder in [
            "scout",
            "planner",
            "integration-tester",
            "backend-reviewer",
            "frontend-reviewer",
            "orchestrator",
            "coordinator",
        ] {
            assert!(
                followup_for_coordinator_inbound(non_builder, &t).is_none(),
                "{non_builder} must not trigger DONE fanout"
            );
        }
    }

    #[test]
    fn followup_skips_approved_from_non_reviewer() {
        // integration-tester is intentionally NOT in this list —
        // since 2026-05-15 (Coordinator MED#1) it's classified as a
        // reviewer and its APPROVED token IS expected to fan out.
        // The positive case is covered by
        // `followup_approved_from_integration_tester_closes_cycle`.
        let t = Transition {
            kind: TransitionKind::Approved,
            task_id: "42".into(),
        };
        for non_reviewer in [
            "scout",
            "planner",
            "backend-builder",
            "frontend-builder",
            "orchestrator",
            "coordinator",
        ] {
            assert!(
                followup_for_coordinator_inbound(non_reviewer, &t).is_none(),
                "{non_reviewer} must not trigger APPROVED fanout"
            );
        }
    }

    #[test]
    fn followup_skips_building_and_changes_needed() {
        let bd = Transition {
            kind: TransitionKind::Building,
            task_id: "42".into(),
        };
        let ch = Transition {
            kind: TransitionKind::ChangesNeeded,
            task_id: "42".into(),
        };
        assert!(followup_for_coordinator_inbound("backend-builder", &bd).is_none());
        assert!(
            followup_for_coordinator_inbound("backend-reviewer", &ch).is_none()
        );
    }

    #[test]
    fn followup_targets_are_reachable_under_static_graph() {
        // Defense-in-depth: every synthesised fanout must be a
        // permitted edge in the static hierarchy graph. If the graph
        // is ever tightened, this test catches the regression before
        // runtime routing fires `Denied`.
        let dn = Transition {
            kind: TransitionKind::Done,
            task_id: "1".into(),
        };
        let ap = Transition {
            kind: TransitionKind::Approved,
            task_id: "1".into(),
        };
        let (t1, _) =
            followup_for_coordinator_inbound("backend-builder", &dn).unwrap();
        assert!(is_allowed("coordinator", &t1));
        let (t2, _) =
            followup_for_coordinator_inbound("frontend-builder", &dn).unwrap();
        assert!(is_allowed("coordinator", &t2));
        let (t3, _) =
            followup_for_coordinator_inbound("backend-reviewer", &ap).unwrap();
        assert!(is_allowed("coordinator", &t3));
        let (t4, _) =
            followup_for_coordinator_inbound("frontend-reviewer", &ap).unwrap();
        assert!(is_allowed("coordinator", &t4));
    }

    // -- end-to-end pipeline ----------------------------------------- //

    #[test]
    fn end_to_end_builder_done_pipeline() {
        // Simulates the bridge calling
        //   parse_lifecycle_token(body)
        //   -> store.record(src_pane, transition)
        //   -> followup_for_coordinator_inbound(src_agent, transition)
        // on a `DONE 42` from backend-builder. The final synthesised
        // fanout must target backend-reviewer and the store must
        // reflect AwaitingReview.
        let store = LifecycleStore::new();
        let body = "DONE 42";
        let transition = parse_lifecycle_token(body).unwrap();
        let new_state = store.record("p-backend-builder", &transition);
        assert_eq!(new_state, LifecycleState::AwaitingReview);
        let followup = followup_for_coordinator_inbound(
            "backend-builder",
            &transition,
        )
        .unwrap();
        assert_eq!(followup.0, "backend-reviewer");
        assert_eq!(followup.1, "review 42");
    }

    #[test]
    fn end_to_end_reviewer_approved_pipeline() {
        let store = LifecycleStore::new();
        let body = "APPROVED 42";
        let transition = parse_lifecycle_token(body).unwrap();
        let new_state = store.record("p-backend-reviewer", &transition);
        assert_eq!(new_state, LifecycleState::Approved);
        let followup =
            followup_for_coordinator_inbound("backend-reviewer", &transition)
                .unwrap();
        assert_eq!(followup.0, "orchestrator");
        assert_eq!(followup.1, "TASK_DONE 42");
        // After the orchestrator-bound fanout is emitted, the bridge
        // calls store.mark_done() to terminalise.
        store.mark_done("p-backend-reviewer", "42");
        assert_eq!(
            store.state_of("p-backend-reviewer", "42"),
            Some(LifecycleState::Done)
        );
    }
}

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

/// Body-prefix the bridge sends the orchestrator when a task is given
/// up: the responsible agent reported `hata —`, or the rework limit
/// ([`MAX_REWORK_ROUNDS`]) ran out. The coordinator may send it too, to
/// abandon a task it cannot get finished. Together with `TASK_DONE` it
/// lets the orchestrator account for every task it dispatched.
pub const TASK_FAILED_PREFIX: &str = "TASK_FAILED ";

/// Body-prefix of the synthesised re-run request sent to the
/// integration-tester once the fix for a task the TESTER rejected has
/// passed review — a reviewer approval alone does not prove the failing
/// test passes now, and the tester alone never reads the fix's code.
pub const RETEST_PREFIX: &str = "retest ";

/// Rework rounds a task gets after `CHANGES_NEEDED`. The rejection
/// after the last round closes the task as failed. The single source of
/// the retry limit — personas quote it, the backend enforces it.
pub const MAX_REWORK_ROUNDS: u8 = 2;

/// The agent whose verdicts span every builder (smoke / retest runs).
pub const TESTER: &str = "integration-tester";

/// The agents the others ask — research and plans. They own no task, so
/// what is watched is that they answer (see [`LifecycleStore::note_route`]).
pub const ADVISORS: &[&str] = &["scout", "planner"];

/// State a single `(source_pane, task_id)` tuple can be in within the
/// autonomy cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// Initial state — the orchestrator or coordinator delivered a
    /// `task_id`-bearing dispatch to a builder or the integration-tester
    /// (see [`LifecycleStore::assign`]) and no signal has come back yet.
    /// Stalls like `Building`, so a dispatch nobody acts on is noticed.
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
    /// Reviewer fired `CHANGES_NEEDED <id>` (the coordinator owes a
    /// rework dispatch — tracked per task, see `take_stalls`), or the
    /// task was given up (`hata`, rework limit, coordinator's
    /// `TASK_FAILED`).
    Failed,
}

impl LifecycleState {
    /// Someone is (supposed to be) working on it: a stall candidate.
    fn in_progress(self) -> bool {
        matches!(self, Self::Assigned | Self::Building)
    }

    fn in_flight(self) -> bool {
        self.in_progress() || self == Self::AwaitingReview
    }
}

/// How a task ended. Once set, no further fanout happens for the id and
/// new dispatches carrying it are refused (ids are single-use).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closure {
    /// `TASK_DONE` went to the orchestrator.
    Done,
    /// `TASK_FAILED` went (or was sent by the coordinator) to the orchestrator.
    Failed,
}

/// What [`LifecycleStore::record`] learned beyond the new state — the
/// inputs [`followup_for_coordinator_inbound`] needs to pick the fanout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub state: LifecycleState,
    /// A domain reviewer's `APPROVED` on a task the integration-tester
    /// rejected: the next step is the tester's `retest`, not `TASK_DONE`.
    pub retest: bool,
    /// That `APPROVED` came while another builder's part of the same id
    /// is still in flight (a tester rejection reworked by both
    /// builders): hold the retest until the last part is approved.
    pub others_in_flight: bool,
    /// For a task the tester rejected: the builders' `DONE` notes of the
    /// current fix round (one per builder). Carried into `retest <id> —
    /// <note>` so the tester knows what changed, and into the final
    /// `TASK_DONE` so the orchestrator's report lists what the rework
    /// touched.
    pub builder_note: Option<String>,
    /// `CHANGES_NEEDED` count for the task so far.
    pub rejections: u8,
    /// This `CHANGES_NEEDED` used up [`MAX_REWORK_ROUNDS`]: the task is
    /// closed as failed.
    pub limit_exceeded: bool,
    /// The task was already closed before this signal; nothing fans out.
    pub already_closed: Option<Closure>,
}

impl Recorded {
    fn plain(state: LifecycleState) -> Self {
        Self {
            state,
            retest: false,
            others_in_flight: false,
            builder_note: None,
            rejections: 0,
            limit_exceeded: false,
            already_closed: None,
        }
    }
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
    (!note.is_empty()).then(|| cap_note(note))
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

/// Pure: the reason text of a `hata — <sebep>` body (capped), or `None`
/// when the body is a bare `hata`.
pub fn error_reason(body: &str) -> Option<String> {
    if !is_error_report(body) {
        return None;
    }
    let head = body.trim().trim_start_matches(is_decoration);
    let reason = head[4..]
        .trim_start_matches(|c| is_separator(c) || matches!(c, '.' | ',' | ';'))
        .trim();
    (!reason.is_empty()).then(|| cap_note(reason))
}

fn cap_note(note: &str) -> String {
    let capped = crate::text::truncate_chars(note, MAX_FOLLOWUP_NOTE_CHARS);
    if capped.len() < note.len() {
        format!("{capped}…")
    } else {
        capped
    }
}

/// Pure: a builder / reviewer / tester body that only means something to
/// the lifecycle (a `DONE` / `APPROVED` / `CHANGES_NEEDED` / `BUILDING`
/// token, or a `task_id`-bearing `hata`) but is addressed to someone
/// other than the coordinator. The bridge only acts on these when they
/// reach the coordinator, so it re-addresses them there instead of
/// letting the chain stall (a `DONE` sent to the orchestrator used to
/// start no review).
pub fn misaddressed_signal(from: &str, to: &str, body: &str, task_id: Option<&str>) -> bool {
    to != "coordinator"
        && (is_builder(from) || is_reviewer(from))
        && is_allowed(from, "coordinator")
        && (parse_lifecycle_token_with_fallback(body, task_id).is_some()
            || (task_id.is_some_and(|id| !id.trim().is_empty()) && is_error_report(body)))
}

/// Pure: a `task_id=<id>` / `task_id: <id>` marker in a dispatch body —
/// the personas teach writing it there AND in the envelope field, and an
/// LLM sometimes fills only the body. The id must contain a digit, like
/// every persona-taught id.
pub fn body_task_id(body: &str) -> Option<&str> {
    let lower = body.to_ascii_lowercase();
    let at = lower.find("task_id")?;
    let rest = body[at + "task_id".len()..]
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '=' | ':' | '`' | '"'));
    let token = rest.split_whitespace().next()?.trim_matches(is_id_trim);
    (!token.is_empty() && token.len() <= MAX_TASK_ID_LEN && has_digit(token)).then_some(token)
}

/// Pure: the task id of a coordinator → orchestrator `TASK_FAILED <id>`
/// report (inline id, else the envelope's `task_id`).
pub fn task_failed_id<'a>(body: &'a str, task_id: Option<&'a str>) -> Option<&'a str> {
    let rest = body.trim().strip_prefix(TASK_FAILED_PREFIX.trim_end())?;
    if rest.chars().next().is_some_and(char::is_alphanumeric) {
        return None;
    }
    inline_id(rest)
        .filter(|id| has_digit(id) || Some(*id) == task_id)
        .or(task_id.map(str::trim).filter(|s| !s.is_empty()))
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

/// Per-session lifecycle store. Pane entries are keyed by
/// `(source_pane_id, task_id)` so a single agent can have several
/// in-flight tasks concurrently (one entry per task) without state
/// collisions; per-task facts (rejections, who rejected, rework owed,
/// closure) live beside them keyed by `task_id` alone.
///
/// The store is owned by the session and read by the bridge watcher
/// (`bridge::synth`) on each delivery. Reads and writes are guarded by a
/// single `Mutex` — the call rate is bounded by the route emit rate
/// (~1 Hz worst case), so contention is not a concern.
///
/// Each entry also remembers WHEN it entered its state and how many
/// stall reminders it has produced, so [`LifecycleStore::take_stalls`]
/// can flag a task stuck in `Assigned` / `Building` / `AwaitingReview`,
/// or a rejection the coordinator never re-dispatched.
#[derive(Default)]
pub struct LifecycleStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    entries: HashMap<(String, String), Entry>,
    tasks: HashMap<String, TaskMeta>,
    /// Set while the stall clocks stand still (see
    /// [`LifecycleStore::pause_clocks`]).
    paused_since: Option<Instant>,
    /// `(advisor, asker)` → the asker's first question the advisor has
    /// not answered yet (see [`ADVISORS`]).
    questions: HashMap<(String, String), Question>,
}

/// An unanswered question to an advisor.
#[derive(Debug, Clone, Copy)]
struct Question {
    since: Instant,
    /// Highest level already reported for it.
    level: u8,
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

#[derive(Debug, Default)]
struct TaskMeta {
    rejections: u8,
    /// The integration-tester rejected it: its fix ends with a retest.
    needs_retest: bool,
    /// `(builder, note)` of each builder's latest `DONE` in the current
    /// fix round — both builders may rework a tester rejection.
    done_notes: Vec<(String, String)>,
    /// Level-2 stall escalations reported so far. A second one (after a
    /// re-dispatch reset the clock) gives the task up.
    escalations: u8,
    /// A rejection the coordinator has not re-dispatched yet.
    rework: Option<Entry>,
    closed: Option<Closure>,
}

impl TaskMeta {
    fn builder_note(&self) -> Option<String> {
        match self.done_notes.as_slice() {
            [] => None,
            [(_, note)] => Some(note.clone()),
            many => Some(
                many.iter()
                    .map(|(agent, note)| format!("@{agent}: {note}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
        }
    }
}

/// How [`LifecycleStore::abandon`] resolved a `hata` report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abandon {
    /// The reporter's own (or judged) in-flight work: closed as failed.
    Closed,
    /// Nothing tracked it (e.g. the dispatch carried no structured
    /// task_id, or it awaited a rework dispatch): closed as failed too —
    /// otherwise the orchestrator would wait for it forever.
    Untracked,
    /// The task is open on someone else's pane: left alone.
    Elsewhere,
    /// Already closed; nothing changes.
    AlreadyClosed(Closure),
}

/// One in-flight task that crossed a stall threshold — output of
/// [`LifecycleStore::take_stalls`]. `level` is 1 at the threshold, 2 at
/// twice the threshold (escalation) and 3 at three times it. A rework the
/// coordinator owes is reported with `state: Failed` and an empty
/// `source_pane`.
///
/// `give_up`: nobody answered the reminder AND the escalation — level 3,
/// or a second escalation after a re-dispatch reset the clock. The store
/// has already closed the task as failed; the caller reports
/// `TASK_FAILED`. Without this a dead pane was reminded, escalated and
/// nudged in a loop forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stall {
    pub source_pane: String,
    pub task_id: String,
    pub state: LifecycleState,
    pub age: Duration,
    pub level: u8,
    pub give_up: bool,
}

/// A question to an advisor that crossed a threshold — output of
/// [`LifecycleStore::take_unanswered`]: `level` 1 at the threshold (the
/// advisor is reminded), 2 at twice it (the asker is told too).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    pub advisor: String,
    pub asker: String,
    pub age: Duration,
    pub level: u8,
}

fn stall_level(age: Duration, threshold: Duration) -> u8 {
    if age >= threshold * 3 {
        3
    } else if age >= threshold * 2 {
        2
    } else if age >= threshold {
        1
    } else {
        0
    }
}

/// Distinct task ids kept before closed ones are dropped entirely
/// (VAL-01). Far above the soft cap: a closed id is all the single-use
/// guard needs, so closed tasks are first shrunk, not forgotten.
const MAX_TASK_IDS: usize = 16 * MAX_LIFECYCLE_ENTRIES;

impl Inner {
    /// Settle every in-flight entry of `task_id` and record the closure.
    fn close(&mut self, task_id: &str, closure: Closure, now: Instant) {
        let state = match closure {
            Closure::Done => LifecycleState::Done,
            Closure::Failed => LifecycleState::Failed,
        };
        for ((_, id), e) in self.entries.iter_mut() {
            if id == task_id && e.state.in_flight() {
                *e = Entry::new(state, now);
            }
        }
        let meta = self.tasks.entry(task_id.to_string()).or_default();
        meta.closed = Some(closure);
        meta.rework = None;
    }

    /// Move every stall clock — the entries, the pending reworks and the
    /// unanswered questions — on by `d`: time that does not count.
    fn credit(&mut self, d: Duration) {
        for e in self.entries.values_mut() {
            e.since += d;
        }
        for meta in self.tasks.values_mut() {
            if let Some(w) = meta.rework.as_mut() {
                w.since += d;
            }
        }
        for q in self.questions.values_mut() {
            q.since += d;
        }
    }

    /// VAL-01: bound memory over a long session. Pane entries that are
    /// settled go first; closed tasks keep their closure (the single-use
    /// guard) and only lose their notes, until the far larger
    /// [`MAX_TASK_IDS`] cap forces them out too.
    fn evict_if_full(&mut self) {
        if self.entries.len() >= MAX_LIFECYCLE_ENTRIES {
            self.entries.retain(|_, e| e.state.in_flight());
        }
        if self.tasks.len() >= MAX_LIFECYCLE_ENTRIES {
            for m in self.tasks.values_mut().filter(|m| m.closed.is_some()) {
                m.done_notes = Vec::new();
            }
        }
        if self.tasks.len() >= MAX_TASK_IDS {
            self.tasks.retain(|_, m| m.closed.is_none());
        }
    }
}

impl LifecycleStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // CONC-01: recover the guard on poison instead of panicking. A
        // poisoned lock only means some *other* holder panicked; the
        // maps themselves are intact.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Apply a coordinator-bound transition from `source_agent` (pane
    /// `source_pane`). `verdict_panes` are the builder panes whose
    /// `AwaitingReview` entries a verdict from `source_agent` settles —
    /// the paired builder for a domain reviewer, both builders for the
    /// tester, none otherwise. `note` is the free text after the token
    /// (kept from a `DONE` for a later retest request). Inserts the entry
    /// if it does not exist.
    pub fn record(
        &self,
        source_agent: &str,
        source_pane: &str,
        transition: &Transition,
        verdict_panes: &[&str],
        note: Option<&str>,
    ) -> Recorded {
        self.record_at(source_agent, source_pane, transition, verdict_panes, note, Instant::now())
    }

    fn record_at(
        &self,
        source_agent: &str,
        source_pane: &str,
        transition: &Transition,
        verdict_panes: &[&str],
        note: Option<&str>,
        now: Instant,
    ) -> Recorded {
        let id = transition.task_id.as_str();
        let key = (source_pane.to_string(), id.to_string());
        let mut g = self.lock();
        g.evict_if_full();
        let prev = g.entries.get(&key).map(|e| e.state);
        let next = apply_transition(prev, transition);
        let mut rec = Recorded::plain(next);
        rec.already_closed = g.tasks.get(id).and_then(|m| m.closed);
        if rec.already_closed.is_some() {
            // A late / duplicate signal for a finished task changes
            // nothing — above all it must not fan out a second time.
            return rec;
        }
        g.entries.insert(key.clone(), Entry::new(next, now));
        // A verdict is recorded under the REVIEWER's pane, but the task it
        // judges sits under the builder's pane in `AwaitingReview`: settle
        // the paired builders' entries (and only theirs — an unrelated
        // builder's task with the same id is not this reviewer's call).
        let verdict = match transition.kind {
            TransitionKind::Approved => Some(LifecycleState::Done),
            TransitionKind::ChangesNeeded => Some(LifecycleState::Failed),
            TransitionKind::Building | TransitionKind::Done => None,
        }
        .filter(|_| is_reviewer(source_agent));
        if let Some(builder_next) = verdict {
            for ((pane, eid), e) in g.entries.iter_mut() {
                if eid == id
                    && verdict_panes.contains(&pane.as_str())
                    && e.state == LifecycleState::AwaitingReview
                {
                    *e = Entry::new(builder_next, now);
                }
            }
        }
        match transition.kind {
            TransitionKind::Done => {
                let meta = g.tasks.entry(id.to_string()).or_default();
                meta.done_notes.retain(|(agent, _)| agent != source_agent);
                if let Some(note) = note {
                    meta.done_notes.push((source_agent.to_string(), note.to_string()));
                }
            }
            // A domain reviewer approved the fix of a task the TESTER
            // rejected: the task stays open until the tester re-runs it.
            TransitionKind::Approved
                if verdict.is_some()
                    && source_agent != TESTER
                    && g.tasks.get(id).is_some_and(|m| m.needs_retest) =>
            {
                rec.retest = true;
                rec.builder_note = g.tasks.get(id).and_then(TaskMeta::builder_note);
                rec.others_in_flight = g
                    .entries
                    .iter()
                    .any(|(k, e)| k != &key && k.1 == id && e.state.in_flight());
            }
            TransitionKind::Approved if verdict.is_some() => {
                // A task closed after a tester rejection: the orchestrator
                // must hear what the fix changed, not only "tests pass".
                rec.builder_note = g
                    .tasks
                    .get(id)
                    .filter(|m| m.needs_retest)
                    .and_then(TaskMeta::builder_note);
                g.close(id, Closure::Done, now);
            }
            TransitionKind::ChangesNeeded if verdict.is_some() => {
                let meta = g.tasks.entry(id.to_string()).or_default();
                meta.rejections = meta.rejections.saturating_add(1);
                if source_agent == TESTER {
                    // A new fix round starts: its notes are what the
                    // next retest is about.
                    meta.needs_retest = true;
                    meta.done_notes.clear();
                }
                rec.rejections = meta.rejections;
                if meta.rejections > MAX_REWORK_ROUNDS {
                    rec.limit_exceeded = true;
                    g.close(id, Closure::Failed, now);
                } else {
                    meta.rework = Some(Entry::new(LifecycleState::Failed, now));
                }
            }
            _ => {}
        }
        rec
    }

    /// A dispatcher delivered a `task_id`-bearing dispatch to the builder
    /// or tester on `pane`: track it as `Assigned` (unless that pane is
    /// already further along with it) and count it as the re-dispatch of
    /// any rework owed. Returns whether an entry was (re)set.
    pub fn assign(&self, pane: &str, task_id: &str) -> bool {
        self.assign_at(pane, task_id, Instant::now())
    }

    fn assign_at(&self, pane: &str, task_id: &str, now: Instant) -> bool {
        let mut g = self.lock();
        g.evict_if_full();
        let meta = g.tasks.entry(task_id.to_string()).or_default();
        if meta.closed.is_some() {
            return false;
        }
        meta.rework = None;
        let key = (pane.to_string(), task_id.to_string());
        if g.entries.get(&key).is_some_and(|e| {
            matches!(e.state, LifecycleState::Building | LifecycleState::AwaitingReview)
        }) {
            return false;
        }
        g.entries.insert(key, Entry::new(LifecycleState::Assigned, now));
        true
    }

    /// Why a dispatcher may NOT hand `task_id` to the agent on
    /// `target_pane`, or `None` when it may. Task ids are single-use and
    /// one task belongs to one builder:
    ///   * a closed id (`TASK_DONE` / `TASK_FAILED` already sent) is
    ///     refused — rework after the end would never be accounted for;
    ///   * an id still open on ANOTHER builder is refused — two builders
    ///     sharing an id make the first verdict close the other's task.
    ///     Exception: a tester rejection may be reworked by both.
    ///
    /// `builders` lists `(agent, pane)` of the builder panes.
    pub fn dispatch_conflict(
        &self,
        target_pane: &str,
        task_id: &str,
        builders: &[(&str, &str)],
    ) -> Option<String> {
        let g = self.lock();
        let meta = g.tasks.get(task_id);
        if let Some(closure) = meta.and_then(|m| m.closed) {
            let how = match closure {
                Closure::Done => "TASK_DONE",
                Closure::Failed => "TASK_FAILED",
            };
            return Some(format!(
                "task `{task_id}` already closed ({how}); task ids are single-use — \
                 give new work a NEW task_id"
            ));
        }
        if meta.is_some_and(|m| m.needs_retest)
            || !builders.iter().any(|(_, p)| *p == target_pane)
        {
            return None;
        }
        builders
            .iter()
            .filter(|(_, pane)| *pane != target_pane)
            .find(|(_, pane)| {
                g.entries
                    .get(&(pane.to_string(), task_id.to_string()))
                    .is_some_and(|e| e.state.in_flight())
            })
            .map(|(agent, _)| {
                format!(
                    "task `{task_id}` is still open on @{agent}; every builder \
                     dispatch needs its own task_id"
                )
            })
    }

    /// An agent gave up on `task_id` (`hata — …` with a task_id): close
    /// the task as failed — so the stall timer stops and the caller can
    /// report `TASK_FAILED` — unless it is open on someone else's pane.
    /// The reporter may end work under `closable_panes` (its own pane,
    /// plus for a reviewer / the tester the panes of the builders it
    /// judges) and work nobody tracks; an unrelated agent's `hata` cannot
    /// end another pane's in-flight task ([`Abandon::Elsewhere`]).
    pub fn abandon(&self, closable_panes: &[&str], task_id: &str) -> Abandon {
        self.abandon_at(closable_panes, task_id, Instant::now())
    }

    fn abandon_at(&self, closable_panes: &[&str], task_id: &str, now: Instant) -> Abandon {
        let mut g = self.lock();
        if let Some(closure) = g.tasks.get(task_id).and_then(|m| m.closed) {
            return Abandon::AlreadyClosed(closure);
        }
        let in_flight = |own: bool| {
            g.entries.iter().any(|((pane, id), e)| {
                id == task_id
                    && e.state.in_flight()
                    && closable_panes.contains(&pane.as_str()) == own
            })
        };
        let outcome = if in_flight(true) {
            Abandon::Closed
        } else if in_flight(false) {
            return Abandon::Elsewhere;
        } else {
            Abandon::Untracked
        };
        g.close(task_id, Closure::Failed, now);
        outcome
    }

    /// The coordinator reported `TASK_FAILED <id>` to the orchestrator:
    /// close the task. Returns false for an unknown or already-closed id.
    pub fn fail_task(&self, task_id: &str) -> bool {
        let mut g = self.lock();
        let known = g.tasks.contains_key(task_id)
            || g.entries.keys().any(|(_, id)| id == task_id);
        if !known || g.tasks.get(task_id).is_some_and(|m| m.closed.is_some()) {
            return false;
        }
        g.close(task_id, Closure::Failed, Instant::now());
        true
    }

    /// How `task_id` ended, if it did.
    pub fn closure(&self, task_id: &str) -> Option<Closure> {
        self.lock().tasks.get(task_id).and_then(|m| m.closed)
    }

    /// Report in-flight tasks that crossed a stall threshold since the
    /// last call: `Assigned` / `Building` older than `build_after`,
    /// `AwaitingReview` older than `review_after`, and a rejection not
    /// re-dispatched within `review_after`. Each reports level 1 once at
    /// the threshold, level 2 once at twice it, and gives the task up
    /// (closing it as failed, [`Stall::give_up`]) at three times it — or
    /// at a second escalation, when a re-dispatch reset the clock in
    /// between. A new transition resets the clock.
    pub fn take_stalls(
        &self,
        now: Instant,
        review_after: Duration,
        build_after: Duration,
    ) -> Vec<Stall> {
        let mut g = self.lock();
        if g.paused_since.is_some() {
            return Vec::new();
        }
        let Inner { entries, tasks, .. } = &mut *g;
        let mut out = Vec::new();
        for ((pane, task_id), e) in entries.iter_mut() {
            let threshold = match e.state {
                LifecycleState::Assigned | LifecycleState::Building => build_after,
                LifecycleState::AwaitingReview => review_after,
                _ => continue,
            };
            let age = now.saturating_duration_since(e.since);
            let level = stall_level(age, threshold);
            if level > e.reminders {
                e.reminders = level;
                let meta = tasks.entry(task_id.clone()).or_default();
                let mut give_up = level >= 3;
                if level == 2 {
                    give_up = meta.escalations >= 1;
                    meta.escalations = meta.escalations.saturating_add(1);
                }
                out.push(Stall {
                    source_pane: pane.clone(),
                    task_id: task_id.clone(),
                    state: e.state,
                    age,
                    level,
                    give_up,
                });
            }
        }
        for (task_id, meta) in tasks.iter_mut() {
            if meta.closed.is_some() {
                continue;
            }
            let Some(w) = meta.rework.as_mut() else {
                continue;
            };
            let age = now.saturating_duration_since(w.since);
            let level = stall_level(age, review_after);
            if level > w.reminders {
                w.reminders = level;
                out.push(Stall {
                    source_pane: String::new(),
                    task_id: task_id.clone(),
                    state: LifecycleState::Failed,
                    age,
                    level,
                    give_up: level >= 3,
                });
            }
        }
        // A task gives up once, whatever stalled first; its other stalls
        // this round are moot.
        out.sort_by(|a, b| a.task_id.cmp(&b.task_id).then(b.give_up.cmp(&a.give_up)));
        let mut given_up: Vec<String> = Vec::new();
        out.retain(|st| {
            if given_up.contains(&st.task_id) {
                return false;
            }
            if st.give_up {
                given_up.push(st.task_id.clone());
            }
            true
        });
        for id in &given_up {
            g.close(id, Closure::Failed, now);
        }
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
        self.lock()
            .entries
            .get(&(source_pane.to_string(), task_id.to_string()))
            .map(|e| e.state)
    }

    /// Number of distinct `(source_pane, task_id)` entries currently
    /// tracked. Exposed for diagnostic / smoke-test use.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// True when no transitions have been recorded yet. Companion to
    /// [`len`] — kept so clippy's `len_without_is_empty` lint is
    /// satisfied and so UI consumers can decide whether to render the
    /// "no in-flight tasks" empty state without a `len() == 0` check.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Stop the stall clocks — e.g. while the account's usage limit is
    /// hit: no agent can work, and every in-flight task would otherwise
    /// be reminded, escalated and finally given up while the user is
    /// away. Idempotent.
    pub fn pause_clocks(&self) {
        self.pause_clocks_at(Instant::now());
    }

    fn pause_clocks_at(&self, now: Instant) {
        let mut g = self.lock();
        if g.paused_since.is_none() {
            g.paused_since = Some(now);
        }
    }

    /// Restart the stall clocks where they stopped: every in-flight
    /// entry (and pending rework) is credited the paused time, so no
    /// task is closer to a reminder than before the pause.
    pub fn resume_clocks(&self) {
        self.resume_clocks_at(Instant::now());
    }

    fn resume_clocks_at(&self, now: Instant) {
        let mut g = self.lock();
        let Some(since) = g.paused_since.take() else {
            return;
        };
        let paused = now.saturating_duration_since(since);
        g.credit(paused);
    }

    /// Time nobody could work in — the machine slept, the process was
    /// frozen (see `bridge::watcher`) — is not stall time either: every
    /// clock is credited `gap`, as a pause would have been. While the
    /// clocks stand still the resume credits it already.
    pub fn credit_gap(&self, gap: Duration) {
        let mut g = self.lock();
        if g.paused_since.is_none() {
            g.credit(gap);
        }
    }

    /// A message `from` → `to` was delivered. An advisor (see
    /// [`ADVISORS`]) that sends anything has spoken: none of its
    /// questions wait any more. A message to an advisor is a question its
    /// sender waits on, timed from the first one not yet answered.
    pub fn note_route(&self, from: &str, to: &str) {
        self.note_route_at(from, to, Instant::now());
    }

    fn note_route_at(&self, from: &str, to: &str, now: Instant) {
        let mut g = self.lock();
        if ADVISORS.contains(&from) {
            g.questions.retain(|(advisor, _), _| advisor != from);
        }
        if ADVISORS.contains(&to) && from != to {
            g.questions
                .entry((to.to_string(), from.to_string()))
                .or_insert(Question { since: now, level: 0 });
        }
    }

    /// Questions to an advisor that crossed `after` since the last call:
    /// level 1 at `after`, level 2 at twice it, each reported once.
    /// Nothing is closed — an advisor owns no task; the asker decides.
    pub fn take_unanswered(&self, now: Instant, after: Duration) -> Vec<Unanswered> {
        let mut g = self.lock();
        if g.paused_since.is_some() {
            return Vec::new();
        }
        let mut out: Vec<Unanswered> = g
            .questions
            .iter_mut()
            .filter_map(|((advisor, asker), q)| {
                let age = now.saturating_duration_since(q.since);
                let level = stall_level(age, after).min(2);
                (level > q.level).then(|| {
                    q.level = level;
                    Unanswered { advisor: advisor.clone(), asker: asker.clone(), age, level }
                })
            })
            .collect();
        out.sort_by(|a, b| (&a.advisor, &a.asker).cmp(&(&b.advisor, &b.asker)));
        out
    }

    /// The agent behind `old` runs in a new pane now (restarted): its
    /// tasks move with it.
    pub fn rename_pane(&self, old: &str, new: &str) {
        let mut g = self.lock();
        let moved: Vec<((String, String), Entry)> = g
            .entries
            .iter()
            .filter(|((pane, _), _)| pane == old)
            .map(|(k, e)| (k.clone(), *e))
            .collect();
        for ((_, task_id), e) in moved {
            g.entries.remove(&(old.to_string(), task_id.clone()));
            g.entries.insert((new.to_string(), task_id), e);
        }
    }

    /// The panes holding an entry for `task_id`, in whatever state —
    /// whose work the task is (closing a task keeps its entries).
    /// Sorted, so an event built from it is stable.
    pub fn holders(&self, task_id: &str) -> Vec<String> {
        let mut panes: Vec<String> = self
            .lock()
            .entries
            .keys()
            .filter(|(_, id)| id == task_id)
            .map(|(pane, _)| pane.clone())
            .collect();
        panes.sort();
        panes.dedup();
        panes
    }

    /// Mark `(source_pane, task_id)` as `Done` regardless of prior
    /// state. Used by the bridge after the synthesised `TASK_DONE`
    /// fanout to the orchestrator has been emitted.
    pub fn mark_done(&self, source_pane: &str, task_id: &str) {
        let key = (source_pane.to_string(), task_id.to_string());
        self.lock()
            .entries
            .insert(key, Entry::new(LifecycleState::Done, Instant::now()));
    }
}

/// Pure: given a coordinator-bound transition fired by `src_agent` and
/// what the store made of it, return the `(target_agent, body)` of the
/// synthesised follow-up route — or `None` if nothing fans out.
///
/// Fanouts:
///
///   * `Done` from a builder → `(<paired reviewer>, "review <id>")`;
///   * `Approved` from a reviewer / the tester → `("orchestrator",
///     "TASK_DONE <id>")` — except for a task the TESTER rejected, whose
///     reviewed fix goes back to it as `("integration-tester",
///     "retest <id>")` once no other part of the id is still in flight;
///   * `ChangesNeeded` that used up the rework limit → `("orchestrator",
///     "TASK_FAILED <id> — …")`.
///
/// `Building` and an ordinary `ChangesNeeded` do NOT fan out: the
/// coordinator received the signal itself, and the rework dispatch
/// needs the reviewer's concrete feedback — a synthesised generic
/// "rework <id>" would drop it (the stall timer reminds the coordinator
/// if the re-dispatch never comes). Nothing fans out for a task that was
/// already closed.
///
/// Hierarchy gate: every fanout target is checked against the
/// `coordinator → target` edge; the bridge's delivery-time
/// `is_allowed` check is a second line of defence.
pub fn followup_for_coordinator_inbound(
    src_agent: &str,
    transition: &Transition,
    rec: &Recorded,
) -> Option<(String, String)> {
    if rec.already_closed.is_some() {
        return None;
    }
    let id = &transition.task_id;
    let (target, body) = match transition.kind {
        // Only builders can fire a DONE that warrants synthesis —
        // anyone else writing `DONE <id>` to coordinator is prose,
        // not a lifecycle signal.
        TransitionKind::Done if is_builder(src_agent) => {
            (reviewer_for_builder(src_agent)?, format!("review {id}"))
        }
        TransitionKind::Approved if is_reviewer(src_agent) && rec.retest => {
            if rec.others_in_flight {
                return None;
            }
            (TESTER, format!("{RETEST_PREFIX}{id}"))
        }
        TransitionKind::Approved if is_reviewer(src_agent) => {
            ("orchestrator", format!("{TASK_DONE_PREFIX}{id}"))
        }
        TransitionKind::ChangesNeeded if is_reviewer(src_agent) && rec.limit_exceeded => (
            "orchestrator",
            format!(
                "{TASK_FAILED_PREFIX}{id} — {}. ret: düzeltme limiti ({MAX_REWORK_ROUNDS} tur) \
                 aşıldı (@{src_agent})",
                rec.rejections
            ),
        ),
        _ => return None,
    };
    is_allowed("coordinator", target).then(|| (target.to_string(), body))
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;

    /// Test shorthand: the agent behind a test pane id, and the builder
    /// panes (`p-builder`, `p-bb`) a verdict settles.
    fn agent_of(pane: &str) -> &str {
        match pane {
            "p-reviewer" | "p-br" => "backend-reviewer",
            "p-builder" | "p-bb" | "p-b" => "backend-builder",
            "p-fb" => "frontend-builder",
            "p-tester" => "integration-tester",
            p => p.trim_start_matches("p-"),
        }
    }

    fn rec_at(s: &LifecycleStore, pane: &str, t: &Transition, now: Instant) -> Recorded {
        s.record_at(agent_of(pane), pane, t, &["p-builder", "p-bb"], None, now)
    }

    fn fresh() -> Recorded {
        Recorded::plain(LifecycleState::Assigned)
    }

    #[test]
    fn paused_clocks_report_no_stalls_and_resume_where_they_stopped() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-builder", "T1", t0);
        let build_after = Duration::from_secs(60);
        // Paused 10 s in: an hour passes, nothing is reported.
        s.pause_clocks_at(t0 + Duration::from_secs(10));
        assert!(s.take_stalls(t0 + Duration::from_secs(3600), build_after, build_after).is_empty());
        // Resumed after that hour: the task is still 10 s old ...
        s.resume_clocks_at(t0 + Duration::from_secs(3610));
        assert!(s.take_stalls(t0 + Duration::from_secs(3620), build_after, build_after).is_empty());
        // ... and stalls only once a minute of RUNNING time has passed.
        let stalls = s.take_stalls(t0 + Duration::from_secs(3661), build_after, build_after);
        assert_eq!(stalls.len(), 1);
        assert_eq!(stalls[0].level, 1);
        // Resuming a running clock changes nothing, and a second pause
        // does not restart the paused time.
        s.resume_clocks_at(t0 + Duration::from_secs(3661));
        s.pause_clocks_at(t0 + Duration::from_secs(3662));
        s.pause_clocks_at(t0 + Duration::from_secs(4000));
        s.resume_clocks_at(t0 + Duration::from_secs(5000));
        // 63 s of running time: no new level yet ...
        assert!(s.take_stalls(t0 + Duration::from_secs(5001), build_after, build_after).is_empty());
        // ... the second one comes at 120 s.
        let stalls = s.take_stalls(t0 + Duration::from_secs(5058), build_after, build_after);
        assert_eq!(stalls.iter().map(|s| s.level).collect::<Vec<_>>(), [2]);
    }

    #[test]
    fn a_gap_the_machine_slept_through_is_not_stall_time() {
        let (rv, bd) = (10 * MIN, 20 * MIN);
        let t0 = Instant::now();
        let sleep = 120 * MIN;
        let woke = t0 + 5 * MIN + sleep;
        // Five minutes into the work: a dispatch (build clock), a review
        // and a rework owed (review clocks).
        let store = || {
            let s = LifecycleStore::new();
            s.assign_at("p-fb", "T1", t0);
            rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T2"), t0);
            rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T3"), t0);
            rec_at(&s, "p-reviewer", &tr(TransitionKind::ChangesNeeded, "T3"), t0);
            s
        };
        // Uncredited, waking two hours on gives every open task up at
        // once, without a single reminder first.
        let woken = store().take_stalls(woke, rv, bd);
        assert!(woken.len() == 3 && woken.iter().all(|x| x.give_up), "{woken:?}");
        // Credited, the work resumes where it stopped, five minutes in.
        let s = store();
        s.credit_gap(sleep);
        assert!(s.take_stalls(woke, rv, bd).is_empty());
        // The review and the rework are reminded five minutes later, the
        // build fifteen — each at level 1, nothing given up.
        let st = s.take_stalls(woke + 5 * MIN, rv, bd);
        let seen: Vec<_> = st.iter().map(|x| (x.task_id.as_str(), x.level, x.give_up)).collect();
        assert_eq!(seen, [("T2", 1, false), ("T3", 1, false)]);
        let st = s.take_stalls(woke + 15 * MIN, rv, bd);
        assert!(st.iter().any(|x| x.task_id == "T1" && x.level == 1 && !x.give_up), "{st:?}");
    }

    #[test]
    fn a_gap_inside_a_hold_is_credited_once_by_the_resume() {
        let (rv, bd) = (10 * MIN, 20 * MIN);
        let t0 = Instant::now();
        let s = LifecycleStore::new();
        s.assign_at("p-fb", "T1", t0);
        s.pause_clocks_at(t0 + MIN);
        // The machine sleeps through the hold; the resume credits all of it.
        s.credit_gap(60 * MIN);
        s.resume_clocks_at(t0 + 61 * MIN);
        // T1 had run a minute: it stalls 19 minutes on, not 79.
        assert!(s.take_stalls(t0 + 79 * MIN, rv, bd).is_empty());
        assert_eq!(s.take_stalls(t0 + 80 * MIN, rv, bd).len(), 1);
    }

    #[test]
    fn an_unanswered_question_reminds_the_advisor_then_tells_the_asker() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.note_route_at("coordinator", "planner", t0);
        let at = |minutes: u32| {
            s.take_unanswered(t0 + minutes * MIN, 10 * MIN)
                .into_iter()
                .map(|q| (q.advisor, q.asker, q.level))
                .collect::<Vec<_>>()
        };
        assert!(at(9).is_empty());
        assert_eq!(at(10), [("planner".to_string(), "coordinator".to_string(), 1)]);
        // Each level once ...
        assert!(at(15).is_empty());
        assert_eq!(at(20), [("planner".to_string(), "coordinator".to_string(), 2)]);
        // ... and nothing after: an advisor owns no task to give up.
        assert!(at(90).is_empty());
    }

    #[test]
    fn an_advisor_that_speaks_answers_its_questions() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        let after = 10 * MIN;
        // Two askers wait on the scout; it speaks, to anyone: both are
        // answered.
        s.note_route_at("backend-builder", "scout", t0);
        s.note_route_at("coordinator", "scout", t0);
        s.note_route_at("scout", "coordinator", t0 + 5 * MIN);
        assert!(s.take_unanswered(t0 + 30 * MIN, after).is_empty());
        // A new question starts a new wait, asking again does not restart
        // it, and messages between the other agents are no questions.
        s.note_route_at("backend-builder", "scout", t0 + 31 * MIN);
        s.note_route_at("backend-builder", "scout", t0 + 39 * MIN);
        s.note_route_at("coordinator", "backend-builder", t0 + 31 * MIN);
        let st = s.take_unanswered(t0 + 41 * MIN, after);
        let seen: Vec<_> = st.iter().map(|q| (q.advisor.as_str(), q.asker.as_str(), q.level)).collect();
        assert_eq!(seen, [("scout", "backend-builder", 1)]);
    }

    #[test]
    fn a_question_waits_out_a_hold_and_a_sleep() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        let after = 10 * MIN;
        s.note_route_at("orchestrator", "planner", t0);
        // Held from minute 1 to 61: nothing is reported meanwhile ...
        s.pause_clocks_at(t0 + MIN);
        assert!(s.take_unanswered(t0 + 30 * MIN, after).is_empty());
        s.resume_clocks_at(t0 + 61 * MIN);
        // ... and the question is a minute old again afterwards.
        assert!(s.take_unanswered(t0 + 69 * MIN, after).is_empty());
        // A two-hour sleep does not count either.
        s.credit_gap(120 * MIN);
        assert!(s.take_unanswered(t0 + 189 * MIN, after).is_empty());
        assert_eq!(s.take_unanswered(t0 + 190 * MIN, after).len(), 1);
    }

    #[test]
    fn a_restarted_agent_keeps_its_tasks() {
        let s = LifecycleStore::new();
        s.assign("p-old", "T1");
        s.assign("p-other", "T2");
        s.rename_pane("p-old", "p-new");
        assert_eq!(s.holders("T1"), ["p-new"]);
        assert_eq!(s.state_of("p-new", "T1"), Some(LifecycleState::Assigned));
        assert_eq!(s.state_of("p-old", "T1"), None);
        assert_eq!(s.holders("T2"), ["p-other"]);
    }

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
        rec_at(&s, "p-backend-builder", &bd, Instant::now());
        rec_at(&s, "p-frontend-builder", &dn, Instant::now());
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
            rec_at(&s, "p-backend-builder", &bd, Instant::now()).state,
            LifecycleState::Building
        );
        assert_eq!(
            rec_at(&s, "p-backend-builder", &dn, Instant::now()).state,
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
        rec_at(&s, "p-backend-builder", &dn, Instant::now());
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
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T1"), t0);

        assert!(s.take_stalls(t0 + 9 * MIN, 10 * MIN, 20 * MIN).is_empty());
        let l1 = s.take_stalls(t0 + 10 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(l1.len(), 1);
        assert_eq!(l1[0].level, 1);
        assert_eq!(l1[0].state, LifecycleState::AwaitingReview);
        // Same level is not reported twice.
        assert!(s.take_stalls(t0 + 15 * MIN, 10 * MIN, 20 * MIN).is_empty());
        let l2 = s.take_stalls(t0 + 20 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(l2[0].level, 2);
        assert!(!l2[0].give_up);
        assert!(s.take_stalls(t0 + 25 * MIN, 10 * MIN, 20 * MIN).is_empty());
        // Three times the threshold with no answer: given up, once.
        let l3 = s.take_stalls(t0 + 30 * MIN, 10 * MIN, 20 * MIN);
        assert!(l3[0].give_up);
        assert!(s.take_stalls(t0 + 90 * MIN, 10 * MIN, 20 * MIN).is_empty());

        // A new transition restarts the clock.
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T2"), t0);
        s.take_stalls(t0 + 10 * MIN, 10 * MIN, 20 * MIN);
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T2"), t0 + 11 * MIN);
        assert!(s.take_stalls(t0 + 15 * MIN, 10 * MIN, 20 * MIN).is_empty());
    }

    #[test]
    fn building_uses_its_own_threshold_and_terminal_states_never_stall() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        rec_at(&s, "p-b", &tr(TransitionKind::Building, "T2"), t0);
        rec_at(&s, "p-r", &tr(TransitionKind::Approved, "T3"), t0);
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
        rec_at(&s, "p-fb", &tr(TransitionKind::Building, "stall-1"), t0);
        // Builder reports `hata —` for its own task.
        assert_eq!(s.abandon_at(&["p-fb"], "stall-1", t0 + MIN), Abandon::Closed);
        assert_eq!(s.state_of("p-fb", "stall-1"), Some(LifecycleState::Failed));
        assert!(s.take_stalls(t0 + 90 * MIN, 10 * MIN, 20 * MIN).is_empty());

        // The paired reviewer's `hata —` settles the builder's
        // AwaitingReview entry (the caller passes the builder's pane).
        rec_at(&s, "p-bb", &tr(TransitionKind::Done, "T8"), t0);
        assert_eq!(s.abandon_at(&["p-br", "p-bb"], "T8", t0), Abandon::Closed);
        assert_eq!(s.state_of("p-bb", "T8"), Some(LifecycleState::Failed));

        // An unrelated agent (e.g. scout) reporting `hata` about the same
        // id can close neither a review wait nor another pane's Building.
        rec_at(&s, "p-bb", &tr(TransitionKind::Done, "T10"), t0);
        assert_eq!(s.abandon_at(&["p-scout"], "T10", t0), Abandon::Elsewhere);
        assert_eq!(s.state_of("p-bb", "T10"), Some(LifecycleState::AwaitingReview));
        rec_at(&s, "p-x", &tr(TransitionKind::Building, "T9"), t0);
        assert_eq!(s.abandon_at(&["p-y"], "T9", t0), Abandon::Elsewhere);
        assert_eq!(s.state_of("p-x", "T9"), Some(LifecycleState::Building));
        assert_eq!(s.closure("T9"), None);
        // A `hata` for a task nobody tracks still closes it (the
        // orchestrator must hear TASK_FAILED), creating no pane entry.
        assert_eq!(s.abandon_at(&["p-fb"], "untracked-1", t0), Abandon::Untracked);
        assert_eq!(s.closure("untracked-1"), Some(Closure::Failed));
        assert_eq!(s.state_of("p-fb", "untracked-1"), None);
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
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T4"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::Approved, "T4"), t0 + MIN);
        assert_eq!(s.state_of("p-builder", "T4"), Some(LifecycleState::Done));
        assert!(s.take_stalls(t0 + 90 * MIN, 10 * MIN, 20 * MIN).is_empty());

        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T5"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::ChangesNeeded, "T5"), t0);
        assert_eq!(s.state_of("p-builder", "T5"), Some(LifecycleState::Failed));
        // Other tasks are untouched.
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T6"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::Approved, "T7"), t0);
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
        let (target, body) = followup_for_coordinator_inbound("backend-builder", &t, &fresh())
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
        let (target, body) = followup_for_coordinator_inbound("frontend-builder", &t, &fresh())
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
                followup_for_coordinator_inbound(reviewer, &t, &fresh()).unwrap();
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
            followup_for_coordinator_inbound("integration-tester", &t, &fresh())
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
                followup_for_coordinator_inbound(non_builder, &t, &fresh()).is_none(),
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
                followup_for_coordinator_inbound(non_reviewer, &t, &fresh()).is_none(),
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
        assert!(followup_for_coordinator_inbound("backend-builder", &bd, &fresh()).is_none());
        assert!(
            followup_for_coordinator_inbound("backend-reviewer", &ch, &fresh()).is_none()
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
            followup_for_coordinator_inbound("backend-builder", &dn, &fresh()).unwrap();
        assert!(is_allowed("coordinator", &t1));
        let (t2, _) =
            followup_for_coordinator_inbound("frontend-builder", &dn, &fresh()).unwrap();
        assert!(is_allowed("coordinator", &t2));
        let (t3, _) =
            followup_for_coordinator_inbound("backend-reviewer", &ap, &fresh()).unwrap();
        assert!(is_allowed("coordinator", &t3));
        let (t4, _) =
            followup_for_coordinator_inbound("frontend-reviewer", &ap, &fresh()).unwrap();
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
        let new_state = rec_at(&store, "p-backend-builder", &transition, Instant::now()).state;
        assert_eq!(new_state, LifecycleState::AwaitingReview);
        let followup = followup_for_coordinator_inbound("backend-builder", &transition, &fresh())
        .unwrap();
        assert_eq!(followup.0, "backend-reviewer");
        assert_eq!(followup.1, "review 42");
    }

    #[test]
    fn end_to_end_reviewer_approved_pipeline() {
        let store = LifecycleStore::new();
        let body = "APPROVED 42";
        let transition = parse_lifecycle_token(body).unwrap();
        let new_state = rec_at(&store, "p-backend-reviewer", &transition, Instant::now()).state;
        assert_eq!(new_state, LifecycleState::Approved);
        let followup =
            followup_for_coordinator_inbound("backend-reviewer", &transition, &fresh())
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

    // -- failure paths (swarm-logic review A1–A6) -------------------- //

    fn ok_followup(agent: &str, t: &Transition, r: &Recorded) -> Option<(String, String)> {
        followup_for_coordinator_inbound(agent, t, r)
    }

    #[test]
    fn a_tester_rejected_fix_is_reviewed_then_retested() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        assert!(s.assign_at("p-tester", "smoke-1", t0));
        let ch = tr(TransitionKind::ChangesNeeded, "smoke-1");
        let r = rec_at(&s, "p-tester", &ch, t0);
        assert_eq!((r.rejections, r.limit_exceeded), (1, false));
        assert!(ok_followup(TESTER, &ch, &r).is_none());

        // Coordinator re-dispatches the fix; the builder's DONE goes to
        // the domain reviewer as usual — the fix's code gets read.
        assert!(s.assign_at("p-builder", "smoke-1", t0));
        let done = tr(TransitionKind::Done, "smoke-1");
        let r = s.record_at("backend-builder", "p-builder", &done, &[], Some("orders.rs fixed"), t0);
        assert_eq!(
            ok_followup("backend-builder", &done, &r),
            Some(("backend-reviewer".to_string(), "review smoke-1".to_string()))
        );

        // The reviewer's APPROVED does NOT close it: the tester re-runs.
        let ap = tr(TransitionKind::Approved, "smoke-1");
        let r = rec_at(&s, "p-reviewer", &ap, t0);
        assert!(r.retest && !r.others_in_flight);
        assert_eq!(r.builder_note.as_deref(), Some("orders.rs fixed"));
        assert_eq!(
            ok_followup("backend-reviewer", &ap, &r),
            Some((TESTER.to_string(), "retest smoke-1".to_string()))
        );
        assert_eq!(s.closure("smoke-1"), None);

        // The tester passes it → TASK_DONE, task closed.
        assert!(s.assign_at("p-tester", "smoke-1", t0));
        let r = rec_at(&s, "p-tester", &ap, t0);
        assert!(!r.retest);
        // TASK_DONE carries what the fix changed.
        assert_eq!(r.builder_note.as_deref(), Some("orders.rs fixed"));
        assert_eq!(
            ok_followup(TESTER, &ap, &r),
            Some(("orchestrator".to_string(), "TASK_DONE smoke-1".to_string()))
        );
        assert_eq!(s.closure("smoke-1"), Some(Closure::Done));
    }

    #[test]
    fn retest_waits_for_every_builder_reworking_the_task() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-tester", "smoke-2", t0);
        rec_at(&s, "p-tester", &tr(TransitionKind::ChangesNeeded, "smoke-2"), t0);
        // A tester rejection may be reworked by both builders at once.
        let builders = [("backend-builder", "p-builder"), ("frontend-builder", "p-fb")];
        assert_eq!(s.dispatch_conflict("p-builder", "smoke-2", &builders), None);
        s.assign_at("p-builder", "smoke-2", t0);
        assert_eq!(s.dispatch_conflict("p-fb", "smoke-2", &builders), None);
        s.assign_at("p-fb", "smoke-2", t0);

        let done = tr(TransitionKind::Done, "smoke-2");
        let ap = tr(TransitionKind::Approved, "smoke-2");
        rec_at(&s, "p-builder", &done, t0);
        // Backend part approved while the frontend part is still being
        // built: no retest yet.
        let r = s.record_at("backend-reviewer", "p-reviewer", &ap, &["p-builder"], None, t0);
        assert!(r.retest && r.others_in_flight);
        assert!(ok_followup("backend-reviewer", &ap, &r).is_none());
        rec_at(&s, "p-fb", &done, t0);
        let r = s.record_at("frontend-reviewer", "p-fr", &ap, &["p-fb"], None, t0);
        assert!(r.retest && !r.others_in_flight);
        assert_eq!(ok_followup("frontend-reviewer", &ap, &r).unwrap().1, "retest smoke-2");
    }

    #[test]
    fn a_retest_carries_every_reworking_builders_note() {
        // Review finding: only the last builder's DONE note survived.
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-tester", "smoke-4", t0);
        rec_at(&s, "p-tester", &tr(TransitionKind::ChangesNeeded, "smoke-4"), t0);
        let done = tr(TransitionKind::Done, "smoke-4");
        s.record_at("backend-builder", "p-builder", &done, &[], Some("parse.rs fixed"), t0);
        s.record_at("frontend-builder", "p-fb", &done, &[], Some("Login.tsx fixed"), t0);
        let ap = tr(TransitionKind::Approved, "smoke-4");
        s.record_at("backend-reviewer", "p-reviewer", &ap, &["p-builder"], None, t0);
        let r = s.record_at("frontend-reviewer", "p-fr", &ap, &["p-fb"], None, t0);
        let note = r.builder_note.expect("note");
        assert!(note.contains("parse.rs fixed") && note.contains("Login.tsx fixed"), "{note}");
        // A builder's newer DONE replaces its own older note.
        s.record_at("backend-builder", "p-builder", &done, &[], Some("parse.rs v2"), t0);
        s.assign_at("p-tester", "smoke-4", t0);
        let r = rec_at(&s, "p-tester", &ap, t0);
        let note = r.builder_note.expect("note");
        assert!(note.contains("parse.rs v2") && !note.contains("parse.rs fixed"), "{note}");
    }

    #[test]
    fn a_stall_nobody_answers_is_given_up_once() {
        // Review finding: remind → escalate → nudge looped forever.
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        let (rv, bd) = (10 * MIN, 20 * MIN);
        // (a) No re-dispatch: level 3 gives up.
        s.assign_at("p-fb", "G1", t0);
        assert!(s.take_stalls(t0 + 20 * MIN, rv, bd).iter().all(|x| !x.give_up));
        assert!(s.take_stalls(t0 + 40 * MIN, rv, bd).iter().all(|x| !x.give_up));
        let st = s.take_stalls(t0 + 60 * MIN, rv, bd);
        assert_eq!(st.len(), 1);
        assert!(st[0].give_up && st[0].level == 3);
        assert_eq!(s.closure("G1"), Some(Closure::Failed));
        assert!(s.take_stalls(t0 + 200 * MIN, rv, bd).is_empty(), "gives up once");

        // (b) The coordinator nudges after the escalation (clock reset):
        // the SECOND escalation gives up instead of looping.
        s.assign_at("p-builder", "G2", t0);
        let st = s.take_stalls(t0 + 40 * MIN, rv, bd);
        assert!(st.iter().any(|x| x.task_id == "G2" && x.level == 2 && !x.give_up));
        s.assign_at("p-builder", "G2", t0 + 41 * MIN);
        let st = s.take_stalls(t0 + 81 * MIN, rv, bd);
        let g2: Vec<_> = st.iter().filter(|x| x.task_id == "G2").collect();
        assert_eq!(g2.len(), 1, "{st:?}");
        assert!(g2[0].give_up);
        assert_eq!(s.closure("G2"), Some(Closure::Failed));

        // (c) An unsent rework gives up at level 3 as well.
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "G3"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::ChangesNeeded, "G3"), t0);
        let st = s.take_stalls(t0 + 30 * MIN, rv, bd);
        assert!(st.iter().any(|x| x.task_id == "G3" && x.give_up), "{st:?}");
    }

    #[test]
    fn closed_ids_stay_single_use_past_the_soft_cap() {
        // Review finding: eviction at MAX_LIFECYCLE_ENTRIES forgot closures.
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-builder", "old-1", t0);
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "old-1"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::Approved, "old-1"), t0);
        for n in 0..MAX_LIFECYCLE_ENTRIES + 10 {
            s.assign_at("p-x", &format!("t{n}"), t0);
        }
        assert_eq!(s.closure("old-1"), Some(Closure::Done));
        assert!(s.dispatch_conflict("p-builder", "old-1", &[]).is_some());
    }

    #[test]
    fn body_task_id_reads_the_persona_taught_marker() {
        assert_eq!(body_task_id("x.rs düzelt. task_id=p3. Bittiğinde DONE p3"), Some("p3"));
        assert_eq!(body_task_id("Task_ID: `fb-7` — yap"), Some("fb-7"));
        assert_eq!(body_task_id("task_id = T12,"), Some("T12"));
        assert_eq!(body_task_id("task_id yok burada"), None);
        assert_eq!(body_task_id("no marker at all"), None);
    }

    #[test]
    fn rework_limit_closes_the_task_as_failed() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        let ch = tr(TransitionKind::ChangesNeeded, "T1");
        for round in 1..=MAX_REWORK_ROUNDS {
            s.assign_at("p-builder", "T1", t0);
            rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T1"), t0);
            let r = rec_at(&s, "p-reviewer", &ch, t0);
            assert_eq!((r.rejections, r.limit_exceeded), (round, false));
            assert!(ok_followup("backend-reviewer", &ch, &r).is_none());
        }
        s.assign_at("p-builder", "T1", t0);
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T1"), t0);
        let r = rec_at(&s, "p-reviewer", &ch, t0);
        assert!(r.limit_exceeded);
        let (to, body) = ok_followup("backend-reviewer", &ch, &r).expect("TASK_FAILED");
        assert_eq!(to, "orchestrator");
        assert!(body.starts_with("TASK_FAILED T1 — 3. ret"), "{body}");
        assert_eq!(s.closure("T1"), Some(Closure::Failed));

        // Nothing fans out for a closed task, and it cannot be re-dispatched.
        let done = tr(TransitionKind::Done, "T1");
        let r = rec_at(&s, "p-builder", &done, t0);
        assert_eq!(r.already_closed, Some(Closure::Failed));
        assert!(ok_followup("backend-builder", &done, &r).is_none());
        assert!(s.dispatch_conflict("p-builder", "T1", &[]).unwrap().contains("TASK_FAILED"));
        assert!(!s.assign_at("p-builder", "T1", t0));
    }

    #[test]
    fn a_verdict_settles_only_the_paired_builders_entry() {
        // A6: frontend-builder's review wait under the same id is not the
        // backend reviewer's call.
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T2"), t0);
        rec_at(&s, "p-fb", &tr(TransitionKind::Done, "T2"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::ChangesNeeded, "T2"), t0);
        assert_eq!(s.state_of("p-builder", "T2"), Some(LifecycleState::Failed));
        assert_eq!(s.state_of("p-fb", "T2"), Some(LifecycleState::AwaitingReview));
    }

    #[test]
    fn one_open_task_id_belongs_to_one_builder() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        let builders = [("backend-builder", "p-builder"), ("frontend-builder", "p-fb")];
        s.assign_at("p-builder", "T3", t0);
        let why = s.dispatch_conflict("p-fb", "T3", &builders).expect("conflict");
        assert!(why.contains("@backend-builder"), "{why}");
        // Same builder again (a clarification / rework) is fine, and the
        // tester is not a builder.
        assert_eq!(s.dispatch_conflict("p-builder", "T3", &builders), None);
        assert_eq!(s.dispatch_conflict("p-tester", "T3", &builders), None);
        // Closed ids are refused for everyone.
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T3"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::Approved, "T3"), t0);
        assert!(s.dispatch_conflict("p-tester", "T3", &builders).unwrap().contains("TASK_DONE"));
    }

    #[test]
    fn an_unanswered_dispatch_stalls_and_a_redispatch_settles_the_rework_wait() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        assert!(s.assign_at("p-fb", "T7", t0));
        assert_eq!(s.state_of("p-fb", "T7"), Some(LifecycleState::Assigned));
        let st = s.take_stalls(t0 + 20 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].state, LifecycleState::Assigned);
        // BUILDING upgrades; a repeated dispatch does not downgrade it.
        rec_at(&s, "p-fb", &tr(TransitionKind::Building, "T7"), t0);
        assert!(!s.assign_at("p-fb", "T7", t0));
        assert_eq!(s.state_of("p-fb", "T7"), Some(LifecycleState::Building));

        // A rejection nobody re-dispatches reminds the coordinator...
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "T8"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::ChangesNeeded, "T8"), t0);
        let st = s.take_stalls(t0 + 10 * MIN, 10 * MIN, 20 * MIN);
        let rework: Vec<_> = st.iter().filter(|x| x.task_id == "T8").collect();
        assert_eq!(rework.len(), 1);
        assert_eq!(rework[0].state, LifecycleState::Failed);
        assert!(rework[0].source_pane.is_empty());
        // ...until the re-dispatch arrives.
        s.assign_at("p-builder", "T8", t0 + 11 * MIN);
        let st = s.take_stalls(t0 + 30 * MIN, 10 * MIN, 20 * MIN);
        assert!(st.iter().all(|x| x.state != LifecycleState::Failed), "{st:?}");
    }

    #[test]
    fn a_retest_run_stalls_on_the_testers_own_entry() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-tester", "smoke-3", t0);
        rec_at(&s, "p-tester", &tr(TransitionKind::ChangesNeeded, "smoke-3"), t0);
        s.assign_at("p-builder", "smoke-3", t0);
        rec_at(&s, "p-builder", &tr(TransitionKind::Done, "smoke-3"), t0);
        rec_at(&s, "p-reviewer", &tr(TransitionKind::Approved, "smoke-3"), t0);
        // The retest dispatch reaches the tester and nothing comes back.
        s.assign_at("p-tester", "smoke-3", t0);
        let st = s.take_stalls(t0 + 25 * MIN, 10 * MIN, 20 * MIN);
        assert_eq!(st.len(), 1, "{st:?}");
        assert_eq!(st[0].source_pane, "p-tester");
        assert_eq!(st[0].state, LifecycleState::Assigned);
    }

    #[test]
    fn hata_closes_an_assigned_task_and_coordinator_can_fail_one() {
        let s = LifecycleStore::new();
        let t0 = Instant::now();
        s.assign_at("p-fb", "T9", t0);
        assert_eq!(s.abandon_at(&["p-fb"], "T9", t0), Abandon::Closed);
        assert_eq!(s.closure("T9"), Some(Closure::Failed));
        assert_eq!(
            s.abandon_at(&["p-fb"], "T9", t0),
            Abandon::AlreadyClosed(Closure::Failed),
            "closes once"
        );

        assert!(!s.fail_task("unknown-1"));
        s.assign_at("p-builder", "T10", t0);
        assert!(s.fail_task("T10"));
        assert_eq!(s.state_of("p-builder", "T10"), Some(LifecycleState::Failed));
        assert!(!s.fail_task("T10"));
    }

    #[test]
    fn misaddressed_signals_are_recognised() {
        assert!(misaddressed_signal("backend-builder", "orchestrator", "DONE T1 — x", None));
        assert!(misaddressed_signal("integration-tester", "backend-builder", "CHANGES_NEEDED s-1 — y", None));
        assert!(misaddressed_signal("frontend-builder", "orchestrator", "hata — yapamadım", Some("T2")));
        // Already at the coordinator, plain replies, and roles that do
        // not own lifecycle signals are left alone.
        assert!(!misaddressed_signal("backend-builder", "coordinator", "DONE T1", None));
        assert!(!misaddressed_signal("backend-builder", "orchestrator", "alındı — başlıyorum", Some("T1")));
        assert!(!misaddressed_signal("backend-builder", "orchestrator", "hata — x", None));
        assert!(!misaddressed_signal("scout", "orchestrator", "DONE T1", None));
    }

    #[test]
    fn task_failed_id_and_error_reason() {
        assert_eq!(task_failed_id("TASK_FAILED T4 — vazgeçildi", None), Some("T4"));
        assert_eq!(task_failed_id("TASK_FAILED — vazgeçildi", Some("T5")), Some("T5"));
        assert_eq!(task_failed_id("TASK_FAILEDX T4", None), None);
        assert_eq!(task_failed_id("özet: TASK_FAILED T4", None), None);
        assert_eq!(error_reason("hata — npm install izni yok").as_deref(), Some("npm install izni yok"));
        assert_eq!(error_reason("**HATA**: x").as_deref(), Some("x"));
        assert_eq!(error_reason("hata"), None);
        assert_eq!(error_reason("tamam"), None);
    }
}

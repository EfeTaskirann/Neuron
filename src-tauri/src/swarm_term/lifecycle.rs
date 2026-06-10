//! Task-lifecycle state machine driving the autonomy contract.
//!
//! Per-task state tracking keyed by `(source_pane_id, task_id)`. The
//! bridge watcher's [`crate::swarm_term::bridge::lifecycle_synthesise`]
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

/// Pure: extract a [`Transition`] from a routed envelope body. Returns
/// `None` if the body doesn't start with one of the four lifecycle
/// prefixes or if the post-prefix tail has no first whitespace-
/// delimited token to use as the task id.
///
/// The body is trimmed before prefix-matching, so leading whitespace
/// in the envelope body does not defeat the match. Anything after the
/// task id
/// (extra notes the agent appended) is discarded — the synthesised
/// follow-up route uses ONLY the id, keeping the routed body short
/// and stable.
pub fn parse_lifecycle_token(body: &str) -> Option<Transition> {
    let body = body.trim();
    for (prefix, kind) in [
        (BUILDING_PREFIX, TransitionKind::Building),
        (DONE_PREFIX, TransitionKind::Done),
        (APPROVED_PREFIX, TransitionKind::Approved),
        (CHANGES_NEEDED_PREFIX, TransitionKind::ChangesNeeded),
    ] {
        if let Some(rest) = body.strip_prefix(prefix) {
            let task_id = rest.split_whitespace().next()?;
            if task_id.is_empty() || task_id.len() > MAX_TASK_ID_LEN {
                return None;
            }
            return Some(Transition {
                kind,
                task_id: task_id.to_string(),
            });
        }
    }
    None
}

/// Like [`parse_lifecycle_token`] but falls back to the structured
/// `Envelope.task_id` field when the body carries a bare lifecycle
/// keyword (e.g. `body: "DONE"`, `task_id: "42"`) with no inline id.
///
/// The primary path — id embedded in the body (`"DONE 42"`) — is tried
/// first and is unchanged. The fallback exists because the persona
/// footer teaches agents to populate BOTH the body token and the
/// envelope's `task_id`; without honouring `task_id`, an agent that
/// filled only the structured field would silently fail to fan out
/// (prefix matched, but the id was dropped).
pub fn parse_lifecycle_token_with_fallback(
    body: &str,
    fallback_task_id: Option<&str>,
) -> Option<Transition> {
    if let Some(t) = parse_lifecycle_token(body) {
        return Some(t);
    }
    let id = fallback_task_id
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= MAX_TASK_ID_LEN)?;
    let trimmed = body.trim();
    for (prefix, kind) in [
        (BUILDING_PREFIX, TransitionKind::Building),
        (DONE_PREFIX, TransitionKind::Done),
        (APPROVED_PREFIX, TransitionKind::Approved),
        (CHANGES_NEEDED_PREFIX, TransitionKind::ChangesNeeded),
    ] {
        if trimmed == prefix.trim_end() {
            return Some(Transition {
                kind,
                task_id: id.to_string(),
            });
        }
    }
    None
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
/// (`bridge::lifecycle_synthesise`) on each coordinator-bound delivery.
/// Reads and writes are guarded by a single `Mutex` — the call rate is
/// bounded by the route emit rate (~1 Hz worst case), so contention is
/// not a concern.
#[derive(Default)]
pub struct LifecycleStore {
    inner: Mutex<HashMap<(String, String), LifecycleState>>,
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
            g.retain(|_, st| {
                !matches!(
                    st,
                    LifecycleState::Approved
                        | LifecycleState::Failed
                        | LifecycleState::Done
                )
            });
        }
        let prev = g.get(&key).copied();
        let next = apply_transition(prev, transition);
        g.insert(key, next);
        next
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
            .copied()
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
            g.insert(key, LifecycleState::Done);
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
        ] {
            assert!(
                parse_lifecycle_token(body).is_none(),
                "body `{body}` must not parse as a lifecycle token"
            );
        }
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

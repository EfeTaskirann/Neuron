//! Static routing graph: who-may-message-whom, plus role-tier
//! grouping consumed by the UI and the lifecycle state machine.
//!
//! The 9 agent ids (kebab-case) match the bundled persona file names
//! under `src/swarm/agents/term/*.md`. v2 may load the graph from
//! `<app_data_dir>/swarm-term/hierarchy.toml`; v1 ships the table
//! hardcoded.
//!
//! **Not a security boundary.** The graph keeps conversations orderly
//! (who reports to whom); it does not contain a hostile agent. All panes
//! share one OS user and file access, so an agent can always write into
//! another agent's outbox and be routed as that agent. Containment lives
//! in the per-role tool permissions (`swarm_term::permissions`) and the
//! paste sanitising in the bridge.
//!
//! # Task lifecycle (autonomy contract)
//!
//! Every user-given goal flows through this fixed cycle of agent
//! hand-offs. The cycle exists so the swarm can complete work without
//! stopping for human approval at any intermediate step:
//!
//! ```text
//!   Orchestrator ──plan + dispatch (task_id)──▶ Builder
//!                                                  │ DONE <task_id>
//!                                                  ▼
//!                                              Coordinator
//!                                                  │ review <task_id>
//!                                                  ▼
//!                                              Reviewer
//!                                                  │ APPROVED <task_id>
//!                                                  ▼
//!                                              Coordinator
//!                                                  │ TASK_DONE <task_id>   (retest <task_id> → tester
//!                                                  │                        first, when the TESTER
//!                                                  │                        rejected the task)
//!                                                  ▼
//!                                              Orchestrator
//! ```
//!
//! Failure side, owned by the coordinator: `CHANGES_NEEDED` → the
//! coordinator re-dispatches the fix (at most
//! `lifecycle::MAX_REWORK_ROUNDS` rounds); `hata` or a used-up limit →
//! `TASK_FAILED <task_id>` to the orchestrator, which accounts for every
//! task as `TASK_DONE` or `TASK_FAILED`.
//!
//! The state machine + autofanout logic lives in
//! [`crate::swarm_term::lifecycle`]. The two key transitions there —
//! `Builder → Coordinator(DONE)` and `Reviewer → Coordinator(APPROVED)`
//! — are SYNTHESISED so the bridge can fan the follow-up route out
//! from coordinator without waiting for the coordinator's claude
//! REPL to type the dispatch itself. That synthesis is what
//! eliminates the human-pause window the user reported on 2026-05-14
//! (the coordinator would sometimes pause to "ask the user if
//! approved", which broke the autonomy contract).
//!
//! Every edge in the cycle is also present in the static [`ALLOWED`]
//! graph — the lifecycle helpers do NOT relax the hierarchy, they
//! just remove a typing step.

pub const AGENT_IDS: &[&str] = &[
    "orchestrator",
    "coordinator",
    "scout",
    "planner",
    "backend-builder",
    "frontend-builder",
    "backend-reviewer",
    "frontend-reviewer",
    "integration-tester",
];

const ALLOWED: &[(&str, &[&str])] = &[
    (
        "orchestrator",
        &[
            "coordinator",
            "scout",
            "planner",
            "backend-builder",
            "frontend-builder",
            "backend-reviewer",
            "frontend-reviewer",
            "integration-tester",
        ],
    ),
    (
        "coordinator",
        &[
            "orchestrator",
            "scout",
            "planner",
            "backend-builder",
            "frontend-builder",
            "backend-reviewer",
            "frontend-reviewer",
            "integration-tester",
        ],
    ),
    // Research tier: scout ↔ planner direct edge is intentional.
    // Planner could already reach scout ("@scout: find X"); without
    // the reverse edge scout couldn't reply to planner's question
    // directly (`@planner: found X`) and was forced through
    // coordinator. Asymmetric was the v1 default; v2 makes it
    // symmetric so the most common research-research handoff stops
    // emitting `route denied` noise and stops adding a hop's latency.
    //
    // Reply edges: every agent that can RECEIVE a dispatch can answer
    // its sender. The orchestrator persona dispatches builders,
    // reviewers and the tester directly, builders ask scout, and the
    // tester reports regressions to builders — without the reverse
    // edges those agents could not send `alındı` / `belirsiz` / `hata`
    // back and the sender waited forever.
    (
        "scout",
        &[
            "coordinator",
            "orchestrator",
            "planner",
            "backend-builder",
            "frontend-builder",
        ],
    ),
    ("planner", &["coordinator", "scout", "orchestrator"]),
    (
        "backend-builder",
        &[
            "scout",
            "backend-reviewer",
            "coordinator",
            "orchestrator",
            "integration-tester",
        ],
    ),
    (
        "frontend-builder",
        &[
            "scout",
            "frontend-reviewer",
            "coordinator",
            "orchestrator",
            "integration-tester",
        ],
    ),
    (
        "backend-reviewer",
        &["backend-builder", "coordinator", "orchestrator"],
    ),
    (
        "frontend-reviewer",
        &["frontend-builder", "coordinator", "orchestrator"],
    ),
    (
        "integration-tester",
        &[
            "backend-builder",
            "frontend-builder",
            "coordinator",
            "orchestrator",
        ],
    ),
];

pub fn is_allowed(src: &str, dst: &str) -> bool {
    ALLOWED
        .iter()
        .find(|(s, _)| *s == src)
        .map(|(_, dsts)| dsts.contains(&dst))
        .unwrap_or(false)
}

pub fn allowed_for(src: &str) -> &'static [&'static str] {
    ALLOWED
        .iter()
        .find(|(s, _)| *s == src)
        .map(|(_, dsts)| *dsts)
        .unwrap_or(&[])
}

// --------------------------------------------------------------------- //
// Role tier grouping (consumed by the UI hierarchy bar + lifecycle).    //
// --------------------------------------------------------------------- //

/// Coarse role-tier grouping of the 9 swarm-term agents.
///
/// `TIERS` is the single source of truth for "which tier does this
/// agent belong to" — consumed by:
///
///   * The frontend hierarchy bar (Builder 2 task) — renders one row
///     per tier with each agent chip in its tier.
///   * The lifecycle state machine — surfaces tier in diagnostic
///     logs so the user can read the cycle without recomputing
///     "is `backend-reviewer` a reviewer?" from the static graph.
///
/// Order matters: the orchestration tier sits at the top of the UI
/// (most senior), build/review at the bottom (executors). Inside a
/// tier the order is the canonical display order.
///
/// **Do NOT** add or remove tiers without also updating the persona
/// files under `src/swarm/agents/term/*.md` — the `id:` frontmatter
/// must stay in sync.
pub const TIERS: &[(&str, &[&str])] = &[
    ("orchestration", &["orchestrator", "coordinator"]),
    ("research", &["scout", "planner"]),
    ("build", &["backend-builder", "frontend-builder"]),
    (
        "review",
        &[
            "backend-reviewer",
            "frontend-reviewer",
            "integration-tester",
        ],
    ),
];

// --------------------------------------------------------------------- //
// Pure agent-role classifiers (used by lifecycle + persona helpers).    //
// --------------------------------------------------------------------- //

/// Map a builder agent id to its paired reviewer. Returns `None` for
/// any agent id that is NOT one of the two builder roles. The pairing
/// is by domain (backend / frontend) so review work stays in the
/// reviewer's area of expertise.
pub fn reviewer_for_builder(builder: &str) -> Option<&'static str> {
    match builder {
        "backend-builder" => Some("backend-reviewer"),
        "frontend-builder" => Some("frontend-reviewer"),
        _ => None,
    }
}

/// The builders whose work a verdict (or a `hata`) from `agent` settles:
/// the paired builder for a domain reviewer, both builders for the
/// integration-tester (its smoke / retest runs span the whole change
/// set), nobody for anyone else.
pub fn builders_judged_by(agent: &str) -> &'static [&'static str] {
    match agent {
        "backend-reviewer" => &["backend-builder"],
        "frontend-reviewer" => &["frontend-builder"],
        "integration-tester" => &["backend-builder", "frontend-builder"],
        _ => &[],
    }
}

/// True iff `agent` hands out `task_id`-bearing work (the lifecycle
/// tracks those dispatches as `Assigned`).
pub fn is_dispatcher(agent: &str) -> bool {
    matches!(agent, "orchestrator" | "coordinator")
}

/// True iff `agent` is a reviewer role. Used by the lifecycle state
/// machine to gate `APPROVED` token synthesis.
///
/// `integration-tester` is included alongside the two domain reviewers
/// because its persona role is final-stage validation: an
/// `APPROVED <task_id>` (to coordinator) from the tester is the
/// canonical "the entire change set passes end-to-end" signal, so the
/// lifecycle machine MUST close the cycle on its approval (auto-fan
/// `TASK_DONE <task_id>` to the orchestrator). This also keeps the
/// reviewer-set symmetric with the frontend's `REVIEWER_AGENTS` set in
/// `app/src/hooks/useRoutingEvents.ts` — UI and backend agree on who
/// counts as a reviewer.
///
/// Note: `reviewer_for_builder` intentionally does NOT pair any
/// builder with `integration-tester` — domain reviewers are domain-
/// matched (backend↔backend, frontend↔frontend), whereas the tester
/// is a global gate that runs AFTER the paired review approves. The
/// `DONE` fanout therefore always targets the domain reviewer; for a
/// task the TESTER rejected, the reviewer's approval of the fix sends
/// it back to the tester as `retest <id>` before `TASK_DONE` (see
/// `lifecycle::followup_for_coordinator_inbound`).
pub fn is_reviewer(agent: &str) -> bool {
    matches!(
        agent,
        "backend-reviewer" | "frontend-reviewer" | "integration-tester"
    )
}

/// True iff `agent` is one of the two builder roles. Mirrors
/// [`reviewer_for_builder`] but as a boolean for call sites that
/// only need the predicate (e.g. lifecycle gating).
pub fn is_builder(agent: &str) -> bool {
    matches!(agent, "backend-builder" | "frontend-builder")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orchestrator_can_reach_all_specialists() {
        for &dst in AGENT_IDS {
            if dst == "orchestrator" {
                continue;
            }
            assert!(
                is_allowed("orchestrator", dst),
                "orchestrator must reach {dst}"
            );
        }
    }

    #[test]
    fn every_edge_has_a_reply_edge() {
        // The H2 regression: a one-way edge lets a message in but no
        // answer out, and the sender waits forever.
        for &(src, dsts) in ALLOWED {
            for &dst in dsts {
                assert!(
                    is_allowed(dst, src),
                    "{src}→{dst} is allowed but {dst} cannot reply"
                );
            }
        }
    }

    #[test]
    fn scout_cannot_reach_reviewers() {
        // Scout answers builders' questions but has no business
        // steering a review.
        assert!(!is_allowed("scout", "backend-reviewer"));
        assert!(!is_allowed("scout", "frontend-reviewer"));
        assert!(!is_allowed("scout", "integration-tester"));
    }

    #[test]
    fn reviewers_stay_in_their_domain() {
        assert!(is_allowed("backend-reviewer", "backend-builder"));
        assert!(is_allowed("backend-reviewer", "coordinator"));
        assert!(is_allowed("backend-reviewer", "orchestrator"));
        assert!(!is_allowed("backend-reviewer", "frontend-builder"));
        assert!(!is_allowed("frontend-reviewer", "backend-builder"));
        assert!(!is_allowed("backend-reviewer", "planner"));
    }

    #[test]
    fn no_self_loops() {
        for &id in AGENT_IDS {
            assert!(!is_allowed(id, id), "self-loop for {id}");
        }
    }

    #[test]
    fn unknown_source_returns_false() {
        assert!(!is_allowed("nobody", "scout"));
    }

    #[test]
    fn allowed_for_returns_list() {
        let scout_allowed = allowed_for("scout");
        assert!(scout_allowed.contains(&"coordinator"));
        assert!(scout_allowed.contains(&"orchestrator"));
        assert!(scout_allowed.contains(&"planner"));
        assert!(scout_allowed.contains(&"backend-builder"));
        assert!(scout_allowed.contains(&"frontend-builder"));
        assert_eq!(scout_allowed.len(), 5);
    }

    #[test]
    fn scout_planner_edge_is_symmetric() {
        assert!(is_allowed("scout", "planner"));
        assert!(is_allowed("planner", "scout"));
    }

    // ----------------------------------------------------------------- //
    // Tier-grouping tests                                               //
    // ----------------------------------------------------------------- //

    #[test]
    fn tiers_cover_every_agent_id_exactly_once() {
        // Critical invariant: every id in AGENT_IDS appears in exactly
        // one tier — no duplicates, no omissions. If this breaks the
        // UI hierarchy bar (Builder 2) shows blank chips and the
        // lifecycle diagnostic logger can't classify routes.
        let mut covered: std::collections::HashSet<&str> =
            std::collections::HashSet::new();
        for (_tier, ids) in TIERS {
            for id in *ids {
                assert!(
                    AGENT_IDS.contains(id),
                    "TIERS lists `{id}` but it's not in AGENT_IDS"
                );
                assert!(
                    covered.insert(*id),
                    "agent `{id}` listed in more than one tier"
                );
            }
        }
        for &id in AGENT_IDS {
            assert!(
                covered.contains(id),
                "agent `{id}` not assigned to any tier"
            );
        }
    }

    // ----------------------------------------------------------------- //
    // Agent-role classifiers                                            //
    // ----------------------------------------------------------------- //

    #[test]
    fn reviewer_for_builder_pairs_by_domain() {
        assert_eq!(
            reviewer_for_builder("backend-builder"),
            Some("backend-reviewer")
        );
        assert_eq!(
            reviewer_for_builder("frontend-builder"),
            Some("frontend-reviewer")
        );
        assert_eq!(reviewer_for_builder("scout"), None);
        assert_eq!(reviewer_for_builder("orchestrator"), None);
        assert_eq!(reviewer_for_builder("backend-reviewer"), None);
    }

    #[test]
    fn is_reviewer_covers_all_three_reviewer_roles() {
        // Domain reviewers + the global integration-tester (final-stage
        // validator). Symmetric with `REVIEWER_AGENTS` in the frontend
        // hook `app/src/hooks/useRoutingEvents.ts` — UI and backend
        // agree on who counts as a reviewer so the lifecycle pill and
        // the APPROVED-fanout machinery don't disagree.
        assert!(is_reviewer("backend-reviewer"));
        assert!(is_reviewer("frontend-reviewer"));
        assert!(is_reviewer("integration-tester"));
        assert!(!is_reviewer("backend-builder"));
        assert!(!is_reviewer("frontend-builder"));
        assert!(!is_reviewer("scout"));
        assert!(!is_reviewer("planner"));
        assert!(!is_reviewer("coordinator"));
        assert!(!is_reviewer("orchestrator"));
    }

    #[test]
    fn is_builder_only_for_builder_roles() {
        assert!(is_builder("backend-builder"));
        assert!(is_builder("frontend-builder"));
        assert!(!is_builder("backend-reviewer"));
        assert!(!is_builder("scout"));
        assert!(!is_builder("orchestrator"));
    }
}

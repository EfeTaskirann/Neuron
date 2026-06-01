//! Static routing graph: who-may-message-whom, plus role-tier
//! grouping consumed by the UI and the lifecycle state machine.
//!
//! The 9 agent ids (kebab-case) match the bundled persona file names
//! under `src/swarm/agents/term/*.md`. v2 may load the graph from
//! `<app_data_dir>/swarm-term/hierarchy.toml`; v1 ships the table
//! hardcoded.
//!
//! # Task lifecycle (autonomy contract)
//!
//! Every user-given goal flows through this fixed cycle of agent
//! hand-offs. The cycle exists so the swarm can complete work without
//! stopping for human approval at any intermediate step:
//!
//! ```text
//!   Orchestrator ──goal──▶ Coordinator
//!                              │
//!                              │ Plan / dispatch
//!                              ▼
//!                          Builder
//!                              │ DONE <task_id>
//!                              ▼
//!                          Coordinator
//!                              │ review <task_id>
//!                              ▼
//!                          Reviewer
//!                              │ APPROVED <task_id>
//!                              ▼
//!                          Coordinator
//!                              │ TASK_DONE <task_id>
//!                              ▼
//!                          Orchestrator
//! ```
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

use serde::{Deserialize, Serialize};
use specta::Type;

/// Wire-stable alias for the agent's kebab-case id. Kept as `String`
/// on the wire so the JSON shape stays trivially interoperable with
/// the frontend.
pub type AgentId = String;

/// Static dump of the routing graph used by the Workflow tab. Captures
/// the full set of agents and every (`from` → `to`) edge present in
/// [`ALLOWED`]. The function [`topology`] builds a fresh snapshot per
/// call; the value is small (9 nodes / 35 edges) so we don't bother
/// caching it.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Topology {
    pub nodes: Vec<AgentId>,
    pub edges: Vec<(AgentId, AgentId)>,
}

/// Build a fresh [`Topology`] snapshot from the compile-time tables.
/// Order is stable: nodes follow [`AGENT_IDS`]; edges iterate
/// [`ALLOWED`] in declaration order so consumers can rely on a
/// deterministic shape without sorting.
pub fn topology() -> Topology {
    let nodes: Vec<AgentId> =
        AGENT_IDS.iter().map(|s| (*s).to_string()).collect();
    let mut edges: Vec<(AgentId, AgentId)> = Vec::new();
    for (src, dsts) in ALLOWED {
        for dst in *dsts {
            edges.push(((*src).to_string(), (*dst).to_string()));
        }
    }
    Topology { nodes, edges }
}

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
    ("scout", &["coordinator", "orchestrator", "planner"]),
    ("planner", &["coordinator", "scout", "orchestrator"]),
    (
        "backend-builder",
        &["scout", "backend-reviewer", "coordinator"],
    ),
    (
        "frontend-builder",
        &["scout", "frontend-reviewer", "coordinator"],
    ),
    ("backend-reviewer", &["backend-builder", "coordinator"]),
    ("frontend-reviewer", &["frontend-builder", "coordinator"]),
    (
        "integration-tester",
        &["backend-builder", "frontend-builder", "coordinator"],
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

/// Look up the tier name for an agent id. Returns `None` for unknown
/// ids — callers should treat this as a programming error (every
/// agent in `AGENT_IDS` is covered by `TIERS`).
pub fn tier_of(agent: &str) -> Option<&'static str> {
    for (tier, ids) in TIERS {
        if ids.contains(&agent) {
            return Some(*tier);
        }
    }
    None
}

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
/// `DONE` fanout therefore still targets the domain reviewer; the
/// tester only ever enters the cycle as the source of an `APPROVED`
/// signal (which it can produce when the orchestrator dispatches it
/// for a smoke run).
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
    fn scout_cannot_reach_builders() {
        assert!(!is_allowed("scout", "backend-builder"));
        assert!(!is_allowed("scout", "frontend-builder"));
    }

    #[test]
    fn reviewers_only_talk_to_their_builder_and_coordinator() {
        assert!(is_allowed("backend-reviewer", "backend-builder"));
        assert!(is_allowed("backend-reviewer", "coordinator"));
        assert!(!is_allowed("backend-reviewer", "frontend-builder"));
        assert!(!is_allowed("backend-reviewer", "orchestrator"));
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
        assert_eq!(scout_allowed.len(), 3);
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

    #[test]
    fn tier_of_returns_expected_groupings() {
        assert_eq!(tier_of("orchestrator"), Some("orchestration"));
        assert_eq!(tier_of("coordinator"), Some("orchestration"));
        assert_eq!(tier_of("scout"), Some("research"));
        assert_eq!(tier_of("planner"), Some("research"));
        assert_eq!(tier_of("backend-builder"), Some("build"));
        assert_eq!(tier_of("frontend-builder"), Some("build"));
        assert_eq!(tier_of("backend-reviewer"), Some("review"));
        assert_eq!(tier_of("frontend-reviewer"), Some("review"));
        assert_eq!(tier_of("integration-tester"), Some("review"));
        assert_eq!(tier_of("nobody"), None);
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

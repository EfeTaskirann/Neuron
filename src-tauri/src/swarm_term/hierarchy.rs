//! Static routing graph: who-may-message-whom.
//!
//! The 9 agent ids (kebab-case) match the bundled persona file names
//! under `src/swarm/agents/term/*.md`. v2 may load the graph from
//! `<app_data_dir>/swarm-term/hierarchy.toml`; v1 ships the table
//! hardcoded.

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
    ("scout", &["coordinator", "orchestrator"]),
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
        assert_eq!(scout_allowed.len(), 2);
    }
}

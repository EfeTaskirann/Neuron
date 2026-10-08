//! `parse_agent_target` unit coverage: prefix stripping, missing
//! prefix rejection, and empty-id rejection.

use super::super::parse_agent_target;

#[test]
fn parse_agent_target_strips_prefix() {
    assert_eq!(parse_agent_target("agent:scout"), Some("scout"));
    assert_eq!(
        parse_agent_target("agent:backend-builder"),
        Some("backend-builder")
    );
}

#[test]
fn parse_agent_target_rejects_missing_prefix() {
    assert_eq!(parse_agent_target("scout"), None);
    assert_eq!(parse_agent_target("pane:p1"), None);
    assert_eq!(parse_agent_target(""), None);
    assert_eq!(parse_agent_target("Agent:scout"), None);
}

#[test]
fn parse_agent_target_rejects_empty_id() {
    assert_eq!(parse_agent_target("agent:"), None);
}

//! `MailboxEvent` round-trip: `kind_str` / serde / `from_row_parts`
//! across all eight variants, plus malformed-payload rejection.

use crate::swarm::mailbox_bus::MailboxEvent;

fn sample_events() -> Vec<MailboxEvent> {
    vec![
        MailboxEvent::TaskDispatch {
            job_id: "j-1".into(),
            target: "agent:scout".into(),
            prompt: "Investigate auth.rs".into(),
            with_help_loop: true,
        },
        MailboxEvent::AgentResult {
            job_id: "j-1".into(),
            agent_id: "scout".into(),
            assistant_text: "Found three matches.".into(),
            total_cost_usd: 0.012_5,
            turn_count: 3,
        },
        MailboxEvent::AgentHelpRequest {
            job_id: "j-1".into(),
            agent_id: "backend-builder".into(),
            reason: "Plan step ambiguous".into(),
            question: "Which struct field carries the user id?".into(),
        },
        MailboxEvent::CoordinatorHelpOutcome {
            job_id: "j-1".into(),
            target_agent_id: "backend-builder".into(),
            outcome_json: r#"{"action":"direct_answer","answer":"User.id"}"#.into(),
        },
        MailboxEvent::JobStarted {
            job_id: "j-1".into(),
            workspace_id: "default".into(),
            goal: "Refactor auth".into(),
        },
        MailboxEvent::JobFinished {
            job_id: "j-1".into(),
            outcome: "done".into(),
            summary: "All approved.".into(),
        },
        MailboxEvent::JobCancel {
            job_id: "j-1".into(),
        },
        MailboxEvent::Note,
    ]
}

#[test]
fn mailbox_event_kind_str_round_trip() {
    for event in sample_events() {
        let kind = event.kind_str();
        let payload_json = serde_json::to_string(&event).unwrap();
        let restored =
            MailboxEvent::from_row_parts(kind, &payload_json).unwrap();
        assert_eq!(restored, event, "round-trip drift on {kind}");
    }
}

#[test]
fn mailbox_event_from_row_parts_handles_each_variant() {
    // Eight variants — one fixture each. The kind_str_round_trip
    // test already covers serde-emitted JSON; this one
    // additionally checks hand-written JSON shapes that the
    // frontend might emit through the IPC.
    let cases: &[(&str, &str, fn(&MailboxEvent) -> bool)] = &[
        (
            "task_dispatch",
            r#"{"kind":"task_dispatch","job_id":"j-1","target":"agent:scout","prompt":"go","with_help_loop":false}"#,
            |e| matches!(e, MailboxEvent::TaskDispatch { with_help_loop: false, .. }),
        ),
        (
            "agent_result",
            r#"{"kind":"agent_result","job_id":"j-1","agent_id":"scout","assistant_text":"done","total_cost_usd":0.5,"turn_count":2}"#,
            |e| matches!(e, MailboxEvent::AgentResult { turn_count: 2, .. }),
        ),
        (
            "agent_help_request",
            r#"{"kind":"agent_help_request","job_id":"j-1","agent_id":"x","reason":"r","question":"q"}"#,
            |e| matches!(e, MailboxEvent::AgentHelpRequest { .. }),
        ),
        (
            "coordinator_help_outcome",
            r#"{"kind":"coordinator_help_outcome","job_id":"j-1","target_agent_id":"x","outcome_json":"{\"action\":\"direct_answer\",\"answer\":\"a\"}"}"#,
            |e| matches!(e, MailboxEvent::CoordinatorHelpOutcome { .. }),
        ),
        (
            "job_started",
            r#"{"kind":"job_started","job_id":"j-1","workspace_id":"default","goal":"g"}"#,
            |e| matches!(e, MailboxEvent::JobStarted { .. }),
        ),
        (
            "job_finished",
            r#"{"kind":"job_finished","job_id":"j-1","outcome":"done","summary":"s"}"#,
            |e| matches!(e, MailboxEvent::JobFinished { .. }),
        ),
        (
            "job_cancel",
            r#"{"kind":"job_cancel","job_id":"j-1"}"#,
            |e| matches!(e, MailboxEvent::JobCancel { .. }),
        ),
        ("note", r#"{}"#, |e| matches!(e, MailboxEvent::Note)),
    ];
    for (kind, payload, predicate) in cases {
        let event = MailboxEvent::from_row_parts(kind, payload)
            .expect(&format!("parse {kind}"));
        assert!(predicate(&event), "predicate failed for kind={kind}");
    }
}

#[test]
fn mailbox_event_from_row_parts_rejects_malformed_payload() {
    // 1) non-JSON garbage
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        "not even close to json",
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 2) JSON object missing required fields
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        r#"{"kind":"task_dispatch","target":"x"}"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 3) JSON array (wrong shape entirely)
    let err = MailboxEvent::from_row_parts(
        "task_dispatch",
        r#"["task_dispatch"]"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");

    // 4) Unknown variant kind
    let err = MailboxEvent::from_row_parts(
        "totally_made_up",
        r#"{"kind":"totally_made_up"}"#,
    )
    .unwrap_err();
    assert_eq!(err.kind(), "internal");
}

#[test]
fn mailbox_event_from_row_parts_handles_empty_payload_with_kind() {
    // Legacy 'note' row with payload_json='{}' (the migration
    // 0010 default) should round-trip to MailboxEvent::Note via
    // the kind splice.
    let event =
        MailboxEvent::from_row_parts("note", "{}").unwrap();
    assert_eq!(event, MailboxEvent::Note);

    // Whitespace also handled.
    let event =
        MailboxEvent::from_row_parts("note", "  ").unwrap();
    assert_eq!(event, MailboxEvent::Note);
}

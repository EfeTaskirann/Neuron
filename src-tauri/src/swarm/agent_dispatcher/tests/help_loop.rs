//! WP-W5-03 `with_help_loop` branch: happy-path help round-trip,
//! the no-help-loop negative case, the `MAX_HELP_ROUNDS` cap, and
//! escalation surfacing as an error result. Drives the specialist
//! through the `ScriptedInvoker` seam.

use super::super::config::MAX_HELP_ROUNDS;
use super::super::*;
use super::wait_for_envelope;
use crate::error::AppError;
use crate::swarm::mailbox_bus::MailboxEvent;
use crate::swarm::transport::InvokeResult;
use crate::test_support::mock_app_with_pool;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Duration as StdDuration};
use tokio::time::sleep;

/// Mock invoker that returns a different `assistant_text` on
/// each consecutive call. Used by the help-loop tests to drive
/// the specialist through "first call emits help block; second
/// call after coordinator answer emits clean result".
struct ScriptedInvoker {
    replies: Arc<StdMutex<Vec<String>>>,
    calls: Arc<StdMutex<Vec<String>>>,
}

impl ScriptedInvoker {
    fn new(replies: Vec<&str>) -> Self {
        Self {
            replies: Arc::new(StdMutex::new(
                replies.into_iter().map(String::from).collect(),
            )),
            calls: Arc::new(StdMutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl AgentInvoker for ScriptedInvoker {
    fn invoke_turn(
        &self,
        _workspace_id: &str,
        _agent_id: &str,
        user_message: &str,
        _timeout: Duration,
        _cancel: Arc<Notify>,
    ) -> impl std::future::Future<
        Output = Result<InvokeResult, AppError>,
    > + Send {
        self.calls
            .lock()
            .unwrap()
            .push(user_message.to_string());
        let replies = Arc::clone(&self.replies);
        async move {
            let mut replies = replies.lock().unwrap();
            if replies.is_empty() {
                return Err(AppError::Internal(
                    "scripted invoker exhausted".into(),
                ));
            }
            let text = replies.remove(0);
            Ok(InvokeResult {
                session_id: "mock-session".to_string(),
                assistant_text: text,
                total_cost_usd: 0.01,
                turn_count: 1,
            })
        }
    }
}

/// Help-loop happy path: specialist emits a help block; the
/// dispatcher emits AgentHelpRequest; we (test) emit
/// CoordinatorHelpOutcome::DirectAnswer; the dispatcher feeds
/// the answer back; specialist returns clean text; dispatcher
/// emits AgentResult.
#[tokio::test]
async fn dispatcher_with_help_loop_routes_via_bus() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(ScriptedInvoker::new(vec![
        // First call: specialist emits a help block.
        r#"{"neuron_help": {"reason": "blocked", "question": "which file?"}}"#,
        // Second call: specialist returns clean text.
        "I built it successfully.",
    ]));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "backend-builder".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Dispatch with help loop on.
    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:backend-builder",
            "build it",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-help".into(),
                target: "agent:backend-builder".into(),
                prompt: "build per plan".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit dispatch");

    // The dispatcher should emit AgentHelpRequest after invoke #1.
    let help_req = wait_for_envelope(&bus, "agent_help_request", |e| {
        matches!(&e.event, MailboxEvent::AgentHelpRequest { agent_id, .. } if agent_id == "backend-builder")
    })
    .await
    .expect("AgentHelpRequest emitted");
    match &help_req.event {
        MailboxEvent::AgentHelpRequest { reason, question, .. } => {
            assert_eq!(reason, "blocked");
            assert_eq!(question, "which file?");
        }
        _ => panic!("unexpected"),
    }

    // (Brain side, simulated by test) — emit CoordinatorHelpOutcome.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:backend-builder",
        "answer",
        Some(help_req.id),
        MailboxEvent::CoordinatorHelpOutcome {
            job_id: "j-help".into(),
            target_agent_id: "backend-builder".into(),
            outcome_json: r#"{"action":"direct_answer","answer":"src/auth.rs"}"#.into(),
        },
    )
    .await
    .expect("emit help outcome");

    // After the dispatcher feeds the answer back, the specialist
    // returns clean text → AgentResult emitted.
    let result_env = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(dispatch_env.id)
    })
    .await
    .expect("AgentResult emitted");
    match &result_env.event {
        MailboxEvent::AgentResult { assistant_text, .. } => {
            assert_eq!(assistant_text, "I built it successfully.");
        }
        _ => panic!("unexpected"),
    }

    // The invoker was called twice: once with the original prompt,
    // once with the coordinator's answer feedback.
    let calls = invoker.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], "build per plan");
    assert!(
        calls[1].contains("Coordinator says:"),
        "second call should be answer feedback: {}",
        calls[1]
    );
    assert!(calls[1].contains("src/auth.rs"));

    dispatcher.shutdown().await;
}

/// `with_help_loop: false` keeps the existing path — no
/// AgentHelpRequest, even if the specialist text contains a
/// `neuron_help` block (the brain parses it client-side).
#[tokio::test]
async fn dispatcher_without_help_loop_does_not_emit_help_request() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(ScriptedInvoker::new(vec![
        r#"{"neuron_help": {"reason": "blocked", "question": "?"}}"#,
    ]));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "backend-builder".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:backend-builder",
            "build",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-nohelp".into(),
                target: "agent:backend-builder".into(),
                prompt: "build".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit dispatch");

    // AgentResult lands directly with the help-block text.
    let result_env = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(dispatch_env.id)
    })
    .await
    .expect("AgentResult emitted");
    match &result_env.event {
        MailboxEvent::AgentResult { assistant_text, .. } => {
            assert!(
                assistant_text.contains("neuron_help"),
                "raw help block expected: {}",
                assistant_text
            );
        }
        _ => panic!("unexpected"),
    }

    // No help_request rows.
    let help_reqs = bus
        .list_typed(Some("agent_help_request"), None, None)
        .await
        .expect("list");
    assert!(help_reqs.is_empty());

    dispatcher.shutdown().await;
}

/// Help-loop respects MAX_HELP_ROUNDS — past 3 iterations the
/// dispatcher emits the most recent assistant_text as the
/// AgentResult so the brain isn't stuck waiting forever.
#[tokio::test]
async fn dispatcher_with_help_loop_respects_max_rounds() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));

    // Specialist returns help blocks indefinitely. Dispatcher
    // will hit MAX_HELP_ROUNDS=3, then do one final invoke.
    // Total invokes = 4 (3 help-loop rounds + 1 final).
    let invoker = Arc::new(ScriptedInvoker::new(vec![
        r#"{"neuron_help": {"reason": "r1", "question": "q1"}}"#,
        r#"{"neuron_help": {"reason": "r2", "question": "q2"}}"#,
        r#"{"neuron_help": {"reason": "r3", "question": "q3"}}"#,
        r#"{"neuron_help": {"reason": "r4", "question": "q4"}}"#,
    ]));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "backend-builder".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:backend-builder",
            "build",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-cap".into(),
                target: "agent:backend-builder".into(),
                prompt: "build".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit dispatch");

    // Test acts as the brain: answer each AgentHelpRequest with
    // a DirectAnswer.
    let answer_brain = {
        let bus = Arc::clone(&bus);
        let app_handle = app.handle().clone();
        tokio::spawn(async move {
            let mut answered = 0;
            let deadline = std::time::Instant::now()
                + StdDuration::from_secs(10);
            while answered < MAX_HELP_ROUNDS as usize
                && std::time::Instant::now() < deadline
            {
                let reqs = bus
                    .list_typed(
                        Some("agent_help_request"),
                        None,
                        Some(50),
                    )
                    .await
                    .expect("list");
                if reqs.len() > answered {
                    let req = &reqs[answered];
                    bus.emit_typed(
                        &app_handle,
                        "default",
                        "agent:coordinator",
                        "agent:backend-builder",
                        "answer",
                        Some(req.id),
                        MailboxEvent::CoordinatorHelpOutcome {
                            job_id: "j-cap".into(),
                            target_agent_id: "backend-builder"
                                .into(),
                            outcome_json:
                                r#"{"action":"direct_answer","answer":"see file"}"#
                                    .into(),
                        },
                    )
                    .await
                    .expect("emit outcome");
                    answered += 1;
                } else {
                    sleep(StdDuration::from_millis(50)).await;
                }
            }
        })
    };

    // Wait for the AgentResult — should land after the cap is hit.
    let result_env = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(dispatch_env.id)
    })
    .await
    .expect("AgentResult emitted post-cap");

    // The result's assistant_text is the FINAL invoke's output.
    // Our script returns help blocks for all calls, so the final
    // text still contains a neuron_help block — the brain can
    // parse this and decide to retry / escalate / finish.
    match &result_env.event {
        MailboxEvent::AgentResult { assistant_text, .. } => {
            assert!(
                assistant_text.contains("neuron_help"),
                "expected help block surfaced post-cap: {}",
                assistant_text
            );
        }
        _ => panic!("unexpected"),
    }

    // Total invoker calls = 4 (3 help rounds + 1 final).
    let calls = invoker.calls();
    assert_eq!(calls.len(), 4);

    let _ = answer_brain.await;
    dispatcher.shutdown().await;
}

/// Help-loop with `escalate` outcome surfaces as an error
/// AgentResult (the W4-05 semantic, owned by the dispatcher since
/// W5-06).
#[tokio::test]
async fn dispatcher_with_help_loop_escalate_surfaces_as_error_result() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(ScriptedInvoker::new(vec![
        r#"{"neuron_help": {"reason": "ambiguous", "question": "OAuth or API key?"}}"#,
    ]));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "backend-builder".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:backend-builder",
            "build",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-esc".into(),
                target: "agent:backend-builder".into(),
                prompt: "build".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit dispatch");

    // Wait for AgentHelpRequest, answer with escalate.
    let help_req = wait_for_envelope(&bus, "agent_help_request", |_| true)
        .await
        .expect("help req");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:backend-builder",
        "escalate",
        Some(help_req.id),
        MailboxEvent::CoordinatorHelpOutcome {
            job_id: "j-esc".into(),
            target_agent_id: "backend-builder".into(),
            outcome_json: r#"{"action":"escalate","user_question":"OAuth or API key?"}"#.into(),
        },
    )
    .await
    .expect("emit outcome");

    // AgentResult surfaces with error: prefix.
    let result_env = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(dispatch_env.id)
    })
    .await
    .expect("AgentResult");
    match &result_env.event {
        MailboxEvent::AgentResult { assistant_text, .. } => {
            assert!(
                assistant_text.starts_with("error:"),
                "escalate -> error result: {}",
                assistant_text
            );
            assert!(assistant_text.contains("escalated to user"));
        }
        _ => panic!("unexpected"),
    }

    dispatcher.shutdown().await;
}

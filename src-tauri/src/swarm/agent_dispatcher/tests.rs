//! Dispatcher unit coverage: target parsing, routing, cancel
//! handling, lagged-receiver recovery, clean shutdown, and the
//! WP-W5-03 help-loop branch. All tests run against a real
//! `MailboxBus` with a closure-based mock of the `AgentInvoker`
//! seam so no `claude` subprocess is spawned.

use super::config::MAX_HELP_ROUNDS;
use super::*;
use crate::error::AppError;
use crate::swarm::mailbox_bus::MailboxEvent;
use crate::swarm::transport::InvokeResult;
use crate::test_support::mock_app_with_pool;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Duration as StdDuration};
use tokio::time::{sleep, timeout};

// ----------------------------------------------------------------
// Mock invoker — closure-based stub that records every call and
// returns a canned result (or signals an error). The
// `wait_for_cancel` flag holds the call until the supplied
// Notify fires, simulating a long-running `claude` turn that the
// dispatcher's JobCancel branch can interrupt.
// ----------------------------------------------------------------

#[derive(Clone)]
struct InvokeCall {
    workspace_id: String,
    agent_id: String,
    user_message: String,
}

enum MockBehavior {
    Ok {
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    Err(String),
    WaitForCancel,
}

struct MockAgentInvoker {
    calls: Arc<StdMutex<Vec<InvokeCall>>>,
    behavior: Arc<Mutex<MockBehavior>>,
}

impl MockAgentInvoker {
    fn new_ok(text: &str) -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            behavior: Arc::new(Mutex::new(MockBehavior::Ok {
                assistant_text: text.to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            })),
        }
    }

    fn new_err(msg: &str) -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            behavior: Arc::new(Mutex::new(MockBehavior::Err(
                msg.to_string(),
            ))),
        }
    }

    fn new_wait_for_cancel() -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
            behavior: Arc::new(Mutex::new(MockBehavior::WaitForCancel)),
        }
    }

    fn calls(&self) -> Vec<InvokeCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl AgentInvoker for MockAgentInvoker {
    fn invoke_turn(
        &self,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        _timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<
        Output = Result<InvokeResult, AppError>,
    > + Send {
        self.calls.lock().unwrap().push(InvokeCall {
            workspace_id: workspace_id.to_string(),
            agent_id: agent_id.to_string(),
            user_message: user_message.to_string(),
        });
        let behavior = Arc::clone(&self.behavior);
        async move {
            let behavior = behavior.lock().await;
            match &*behavior {
                MockBehavior::Ok {
                    assistant_text,
                    total_cost_usd,
                    turn_count,
                } => Ok(InvokeResult {
                    session_id: "mock-session".to_string(),
                    assistant_text: assistant_text.clone(),
                    total_cost_usd: *total_cost_usd,
                    turn_count: *turn_count,
                }),
                MockBehavior::Err(msg) => {
                    Err(AppError::SwarmInvoke(msg.clone()))
                }
                MockBehavior::WaitForCancel => {
                    drop(behavior);
                    cancel.notified().await;
                    Err(AppError::Cancelled(
                        "cancelled by test".into(),
                    ))
                }
            }
        }
    }
}

// ----------------------------------------------------------------
// parse_agent_target tests
// ----------------------------------------------------------------

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

// ----------------------------------------------------------------
// Dispatcher routing
// ----------------------------------------------------------------

/// Helper: poll the bus's mailbox table for a row matching the
/// predicate, with a soft timeout so the test fails fast rather
/// than hanging if the dispatcher never emits.
async fn wait_for_envelope<F>(
    bus: &MailboxBus,
    kind: &str,
    predicate: F,
) -> Option<MailboxEnvelope>
where
    F: Fn(&MailboxEnvelope) -> bool,
{
    let deadline =
        std::time::Instant::now() + StdDuration::from_secs(5);
    loop {
        let rows = bus
            .list_typed(Some(kind), None, Some(500))
            .await
            .expect("list ok");
        if let Some(env) =
            rows.into_iter().find(|env| predicate(env))
        {
            return Some(env);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        sleep(StdDuration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn dispatcher_routes_matching_target() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker =
        Arc::new(MockAgentInvoker::new_ok("planner says hi"));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Dispatch matching target.
    let env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "kick off",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-1".into(),
                target: "agent:planner".into(),
                prompt: "Plan the build".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit dispatch");

    // Wait for the AgentResult emit.
    let result = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(env.id)
    })
    .await
    .expect("agent result emitted");

    match &result.event {
        MailboxEvent::AgentResult {
            job_id,
            agent_id,
            assistant_text,
            turn_count,
            ..
        } => {
            assert_eq!(job_id, "j-1");
            assert_eq!(agent_id, "planner");
            assert_eq!(assistant_text, "planner says hi");
            assert_eq!(*turn_count, 1);
        }
        _ => panic!("unexpected event variant"),
    }

    let calls = invoker.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].workspace_id, "default");
    assert_eq!(calls[0].agent_id, "planner");
    assert_eq!(calls[0].user_message, "Plan the build");

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_ignores_non_matching_target() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(MockAgentInvoker::new_ok("planner"));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Emit dispatches for OTHER agents.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:scout",
        "wrong target",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-1".into(),
            target: "agent:scout".into(),
            prompt: "Investigate".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "pane:p1",
        "non-agent prefix",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-2".into(),
            target: "pane:p1".into(),
            prompt: "Hello".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:",
        "empty id",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-3".into(),
            target: "agent:".into(),
            prompt: "Hello".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");

    // Give the dispatcher a moment to process and ignore.
    sleep(StdDuration::from_millis(150)).await;

    // No AgentResult should have been emitted.
    let results =
        bus.list_typed(Some("agent_result"), None, None).await.unwrap();
    assert!(results.is_empty(), "no results expected: {results:?}");
    assert!(invoker.calls().is_empty());

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_emits_agent_result_with_parent_id() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker =
        Arc::new(MockAgentInvoker::new_ok("ok ok"));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "scout".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    let env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:scout",
            "go",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-42".into(),
                target: "agent:scout".into(),
                prompt: "hi".into(),
                with_help_loop: true,
            },
        )
        .await
        .expect("emit");

    let result = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(env.id)
    })
    .await
    .expect("agent result emitted");

    // parent_id chains back to the dispatch row.
    assert_eq!(result.parent_id, Some(env.id));
    assert_eq!(result.from_pane, "agent:scout");
    assert_eq!(result.to_pane, "agent:coordinator");
    if let MailboxEvent::AgentResult {
        assistant_text,
        total_cost_usd,
        ..
    } = &result.event
    {
        assert_eq!(assistant_text, "ok ok");
        assert!((*total_cost_usd - 0.01).abs() < 1e-9);
    } else {
        panic!("unexpected variant");
    }

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_emits_error_result_on_invoke_failure() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker =
        Arc::new(MockAgentInvoker::new_err("subprocess died"));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    let env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "go",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-err".into(),
                target: "agent:planner".into(),
                prompt: "explode".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");

    let result = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(env.id)
    })
    .await
    .expect("agent result emitted");

    match &result.event {
        MailboxEvent::AgentResult {
            assistant_text,
            total_cost_usd,
            turn_count,
            ..
        } => {
            assert!(
                assistant_text.starts_with("error:"),
                "unexpected: {assistant_text}"
            );
            assert!(assistant_text.contains("subprocess died"));
            assert_eq!(*total_cost_usd, 0.0_f64);
            assert_eq!(*turn_count, 0);
        }
        _ => panic!("unexpected variant"),
    }

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_cancels_in_flight_invoke_on_job_cancel() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(MockAgentInvoker::new_wait_for_cancel());

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Kick off a dispatch — invoke will block on cancel notify.
    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "go",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-cancel".into(),
                target: "agent:planner".into(),
                prompt: "long".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit");

    // Wait briefly for the invoker to actually be called (so
    // the slot is populated before the cancel races in).
    let deadline =
        std::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        if !invoker.calls().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("invoker never called");
        }
        sleep(StdDuration::from_millis(10)).await;
    }

    // Now signal cancel.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:planner",
        "cancel",
        None,
        MailboxEvent::JobCancel {
            job_id: "j-cancel".into(),
        },
    )
    .await
    .expect("emit cancel");

    // The cancelled invoke surfaces an error AgentResult.
    let result = timeout(
        StdDuration::from_secs(5),
        wait_for_envelope(&bus, "agent_result", |e| {
            e.parent_id == Some(dispatch_env.id)
        }),
    )
    .await
    .expect("timeout waiting for cancel result");
    let env = result.expect("agent result emitted");
    match &env.event {
        MailboxEvent::AgentResult {
            assistant_text, ..
        } => {
            assert!(
                assistant_text.starts_with("error:"),
                "expected error result on cancel: {assistant_text}"
            );
        }
        _ => panic!("unexpected variant"),
    }

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_ignores_job_cancel_for_other_job() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(MockAgentInvoker::new_wait_for_cancel());

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Kick off j-A.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:coordinator",
        "agent:planner",
        "go",
        None,
        MailboxEvent::TaskDispatch {
            job_id: "j-A".into(),
            target: "agent:planner".into(),
            prompt: "long".into(),
            with_help_loop: false,
        },
    )
    .await
    .expect("emit");

    // Wait for the invoker to be in flight.
    let deadline =
        std::time::Instant::now() + StdDuration::from_secs(2);
    loop {
        if !invoker.calls().is_empty() {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("invoker never called");
        }
        sleep(StdDuration::from_millis(10)).await;
    }

    // Cancel a *different* job.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:user",
        "agent:planner",
        "cancel",
        None,
        MailboxEvent::JobCancel {
            job_id: "j-B".into(),
        },
    )
    .await
    .expect("emit cancel");

    // Give the dispatcher a chance to process the cancel; the
    // invoker MUST still be blocked because the cancel didn't
    // match its job_id.
    sleep(StdDuration::from_millis(200)).await;
    let no_results =
        bus.list_typed(Some("agent_result"), None, None).await.unwrap();
    assert!(
        no_results.is_empty(),
        "no agent_result expected (still in flight): {no_results:?}"
    );

    // shutdown drains the in-flight invoke so the test exits
    // promptly.
    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_handles_lagged_receiver() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));

    // Pre-subscribe so emits land on a real receiver. Holding
    // this `extra_rx` makes the channel "active".
    let mut extra_rx = bus.subscribe("default").await;

    let invoker = Arc::new(MockAgentInvoker::new_ok("survived"));

    // The dispatcher subscribes here, then we burn its receive
    // loop with > 64 events all emitted before it gets a chance
    // to drain.
    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // Flood with 200 unrelated notes — well past the
    // BROADCAST_CAPACITY (64). The dispatcher will see
    // RecvError::Lagged and warn.
    for i in 0..200 {
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:noise",
            "agent:noise",
            &format!("flood {i}"),
            None,
            MailboxEvent::Note,
        )
        .await
        .expect("emit note");
    }
    // Drain the secondary receiver so its buffer doesn't keep
    // backpressure (broadcast doesn't actually backpressure on
    // send — slow consumers see Lagged on next recv).
    while extra_rx.try_recv().is_ok() {}

    // Now emit a genuine dispatch. The dispatcher should
    // process it post-lag.
    let dispatch_env = bus
        .emit_typed(
            app.handle(),
            "default",
            "agent:coordinator",
            "agent:planner",
            "post-lag",
            None,
            MailboxEvent::TaskDispatch {
                job_id: "j-postlag".into(),
                target: "agent:planner".into(),
                prompt: "post-lag prompt".into(),
                with_help_loop: false,
            },
        )
        .await
        .expect("emit dispatch");

    let result = wait_for_envelope(&bus, "agent_result", |e| {
        e.parent_id == Some(dispatch_env.id)
    })
    .await
    .expect("dispatcher recovered from lag and processed dispatch");
    match &result.event {
        MailboxEvent::AgentResult { assistant_text, .. } => {
            assert_eq!(assistant_text, "survived");
        }
        _ => panic!("unexpected variant"),
    }

    dispatcher.shutdown().await;
}

#[tokio::test]
async fn dispatcher_shutdown_drains_cleanly() {
    let (app, pool, _dir) = mock_app_with_pool().await;
    let bus = Arc::new(MailboxBus::new(pool.clone()));
    let invoker = Arc::new(MockAgentInvoker::new_ok("ok"));

    let dispatcher = MailboxAgentDispatcher::spawn(
        app.handle().clone(),
        "default".into(),
        "planner".into(),
        Arc::clone(&invoker),
        Arc::clone(&bus),
    )
    .await;

    // shutdown returns within a reasonable time even with no
    // events in flight.
    timeout(StdDuration::from_secs(2), dispatcher.shutdown())
        .await
        .expect("shutdown drained within 2s");

    // The bus is still usable post-shutdown — emits don't panic.
    bus.emit_typed(
        app.handle(),
        "default",
        "agent:noise",
        "agent:noise",
        "post shutdown",
        None,
        MailboxEvent::Note,
    )
    .await
    .expect("post-shutdown emit ok");
}

// ----------------------------------------------------------------
// WP-W5-03 — with_help_loop branch
// ----------------------------------------------------------------

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

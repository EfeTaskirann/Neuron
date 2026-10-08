//! Dispatcher routing matrix: matching/non-matching target routing,
//! `AgentResult` parent-id linkage, error-result emission, in-flight
//! cancel handling, lagged-receiver recovery, and clean shutdown.
//! Drives the dispatcher through the closure-based `MockAgentInvoker`
//! seam.

use super::super::*;
use super::wait_for_envelope;
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

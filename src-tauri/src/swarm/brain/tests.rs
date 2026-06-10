//! Unit + run-loop tests for the brain. Moved verbatim from the
//! monolithic `brain.rs` `#[cfg(test)] mod tests` block; the imports
//! below replace the names the in-file `use super::*` used to pull
//! from the (now split) top-level module — `MAX_DISPATCHES_ENV` moved
//! into the `action` submodule, and `InvokeResult` / `MailboxEnvelope`
//! are now named only by the tests.

use super::action::MAX_DISPATCHES_ENV;
use crate::swarm::mailbox_bus::MailboxEnvelope;
use crate::swarm::transport::InvokeResult;

    use super::*;
    use crate::test_support::mock_app_with_pool;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration as StdDuration;
    use tokio::sync::Mutex;
    use tokio::time::timeout;

    // ----------------------------------------------------------------
    // Parser tests (≥ 8)
    // ----------------------------------------------------------------

    #[test]
    fn parse_dispatch_action_basic() {
        let text = r#"{"action":"dispatch","target":"agent:scout","prompt":"go","with_help_loop":true}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch {
                target,
                prompt,
                with_help_loop,
            } => {
                assert_eq!(target, "agent:scout");
                assert_eq!(prompt, "go");
                assert!(with_help_loop);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_multibyte_char_straddling_scan_cap_does_not_panic() {
        // 'ü' is 2 bytes: the odd ASCII prefix forces a char to straddle
        // the 16 KiB scan cap. Must error gracefully, not panic.
        let huge = format!("x{}", "ü".repeat(32 * 1024));
        assert!(parse_brain_action(&huge).is_err());
    }

    #[test]
    fn parse_dispatch_action_with_default_help_loop() {
        // `with_help_loop` omitted — serde default = false.
        let text = r#"{"action":"dispatch","target":"agent:planner","prompt":"plan it"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch {
                with_help_loop, ..
            } => {
                assert!(!with_help_loop);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_finish_action_done() {
        let text = r#"{"action":"finish","outcome":"done","summary":"all approved"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, summary } => {
                assert_eq!(outcome, "done");
                assert_eq!(summary, "all approved");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_finish_action_failed() {
        let text = r#"{"action":"finish","outcome":"failed","summary":"reviewer rejected"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, .. } => {
                assert_eq!(outcome, "failed");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_ask_user_action() {
        let text = r#"{"action":"ask_user","question":"OAuth or API key?"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::AskUser { question } => {
                assert_eq!(question, "OAuth or API key?");
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_direct_answer() {
        // body_json carries a serialised CoordinatorHelpOutcome
        // (DirectAnswer variant). Must NOT be parsed back as a
        // BrainAction itself — the brain treats body_json as opaque.
        let text = r#"{"action":"help_outcome","target":"agent:scout","body_json":"{\"action\":\"direct_answer\",\"answer\":\"use OAuth\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { target, body_json } => {
                assert_eq!(target, "agent:scout");
                assert!(body_json.contains("direct_answer"));
                assert!(body_json.contains("use OAuth"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_ask_back() {
        let text = r#"{"action":"help_outcome","target":"agent:planner","body_json":"{\"action\":\"ask_back\",\"followup_question\":\"which file?\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { body_json, .. } => {
                assert!(body_json.contains("ask_back"));
                assert!(body_json.contains("which file?"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_help_outcome_action_escalate() {
        let text = r#"{"action":"help_outcome","target":"agent:backend-builder","body_json":"{\"action\":\"escalate\",\"user_question\":\"OAuth or API key?\"}"}"#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::HelpOutcome { body_json, .. } => {
                assert!(body_json.contains("escalate"));
            }
            other => panic!("expected HelpOutcome, got {other:?}"),
        }
    }

    #[test]
    fn parse_handles_fenced_json_block() {
        let text = "Some preamble.\n\n```json\n{\"action\":\"finish\",\"outcome\":\"done\",\"summary\":\"all good\"}\n```\n\nTrailing.";
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Finish { outcome, .. } => {
                assert_eq!(outcome, "done");
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[test]
    fn parse_handles_first_balanced_object() {
        // Realistic LLM dump — prose around the JSON, no fence.
        let text = r#"OK. {"action":"dispatch","target":"agent:scout","prompt":"investigate"} let's go."#;
        let action = parse_brain_action(text).expect("parsed");
        match action {
            BrainAction::Dispatch { target, .. } => {
                assert_eq!(target, "agent:scout");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn parse_rejects_unknown_action() {
        let text = r#"{"action":"do_a_thing","target":"agent:scout"}"#;
        let err = parse_brain_action(text)
            .expect_err("unknown action rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    #[test]
    fn parse_rejects_malformed_json() {
        let err = parse_brain_action("Just prose, no JSON at all.")
            .expect_err("missing JSON rejected");
        assert_eq!(err.kind(), "swarm_invoke");

        let err = parse_brain_action("not even close")
            .expect_err("garbage rejected");
        assert_eq!(err.kind(), "swarm_invoke");
    }

    #[test]
    fn resolve_max_dispatches_default() {
        let prior = std::env::var(MAX_DISPATCHES_ENV).ok();
        std::env::remove_var(MAX_DISPATCHES_ENV);
        assert_eq!(resolve_max_dispatches(), DEFAULT_MAX_DISPATCHES);
        if let Some(v) = prior {
            std::env::set_var(MAX_DISPATCHES_ENV, v);
        }
    }

    // ----------------------------------------------------------------
    // Mock invoker for run-loop tests.
    // ----------------------------------------------------------------

    /// Drives the brain through a scripted sequence of
    /// `assistant_text` replies. Each call to
    /// `invoke_coordinator_turn` pops the next reply from the
    /// front of the script.
    struct ScriptedCoordinatorInvoker {
        script: Arc<Mutex<Vec<MockReply>>>,
        calls: Arc<StdMutex<Vec<String>>>,
    }

    enum MockReply {
        AssistantText(String),
        Error(AppError),
        /// Reserved for tests that block the coordinator turn until
        /// a `cancel` signal fires — none of the W5-03 tests
        /// currently use this branch (the cancel test fires after
        /// the brain enters the wait-for-event step), but keep it
        /// available for the integration-smoke shape.
        #[allow(dead_code)]
        WaitForCancel,
    }

    impl ScriptedCoordinatorInvoker {
        fn new(replies: Vec<MockReply>) -> Self {
            Self {
                script: Arc::new(Mutex::new(replies)),
                calls: Arc::new(StdMutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl CoordinatorInvoker for ScriptedCoordinatorInvoker {
        fn invoke_coordinator_turn(
            &self,
            _workspace_id: &str,
            user_message: &str,
            _timeout: Duration,
            cancel: Arc<Notify>,
        ) -> impl std::future::Future<
            Output = Result<InvokeResult, AppError>,
        > + Send {
            self.calls.lock().unwrap().push(user_message.to_string());
            let script = Arc::clone(&self.script);
            async move {
                let mut script = script.lock().await;
                if script.is_empty() {
                    return Err(AppError::Internal(
                        "scripted invoker exhausted".into(),
                    ));
                }
                let reply = script.remove(0);
                drop(script);
                match reply {
                    MockReply::AssistantText(text) => Ok(InvokeResult {
                        session_id: "mock-session".to_string(),
                        assistant_text: text,
                        total_cost_usd: 0.01,
                        turn_count: 1,
                    }),
                    MockReply::Error(e) => Err(e),
                    MockReply::WaitForCancel => {
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
    // Helpers for run-loop tests.
    // ----------------------------------------------------------------

    /// Helper: build a mailbox bus and the supporting glue for a
    /// brain test. Tests own the brain spawn itself so they can
    /// pin `max_dispatches` without rebuilding the harness.
    /// `_invoker` and `_max_dispatches` are accepted (and unused)
    /// for call-site readability — the same shape stays uniform
    /// across every test.
    async fn setup_brain<I: CoordinatorInvoker>(
        _invoker: Arc<I>,
        _max_dispatches: u32,
    ) -> (
        tauri::App<tauri::test::MockRuntime>,
        Arc<MailboxBus>,
        Arc<Notify>,
        String,
        String,
        tempfile::TempDir,
    ) {
        let (app, pool, dir) = mock_app_with_pool().await;
        let bus = Arc::new(MailboxBus::new(pool));
        let cancel = Arc::new(Notify::new());
        let workspace_id = "default".to_string();
        let job_id = "j-test".to_string();
        (app, bus, cancel, workspace_id, job_id, dir)
    }

    /// Wait for an envelope matching `predicate` to appear on the
    /// bus's persisted log. Bounded soft-timeout so tests fail fast
    /// rather than hanging on a stuck brain.
    async fn wait_for_envelope_log<F>(
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
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
    }

    // ----------------------------------------------------------------
    // Run-loop tests (≥ 12)
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn brain_emits_first_dispatch_after_job_started() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"investigate auth.rs","with_help_loop":false}"#
                    .to_string(),
            ),
            // Stop the loop after the dispatch by returning a
            // finish on the next call (the test will inject an
            // AgentResult so the brain reaches the second turn).
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let ws_for_brain = ws.clone();
        let job_for_brain = job_id.clone();
        let invoker_for_brain = Arc::clone(&invoker);
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws_for_brain,
                job_for_brain,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // 1. JobStarted appears.
        let started = wait_for_envelope_log(&bus, "job_started", |e| {
            matches!(&e.event, MailboxEvent::JobStarted { job_id: jid, .. } if jid == &job_id)
        })
        .await
        .expect("JobStarted emitted");
        match &started.event {
            MailboxEvent::JobStarted { goal, .. } => {
                assert_eq!(goal, "test goal");
            }
            _ => panic!("unexpected"),
        }

        // 2. The first dispatch lands.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { job_id: jid, target, .. } if jid == &job_id && target == "agent:scout")
        })
        .await
        .expect("first dispatch emitted");
        match &dispatch.event {
            MailboxEvent::TaskDispatch { prompt, .. } => {
                assert_eq!(prompt, "investigate auth.rs");
            }
            _ => panic!("unexpected"),
        }

        // 3. Inject an AgentResult so the brain reaches its second turn.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "scout result",
            Some(dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                assistant_text: "I found three matches.".to_string(),
                total_cost_usd: 0.012,
                turn_count: 1,
            },
        )
        .await
        .expect("emit AgentResult");

        // 4. The brain emits Finish and returns.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain result");
        assert_eq!(result.outcome, "done");
        assert_eq!(result.job_id, job_id);

        // The second invoke saw the AgentResult-shaped prompt.
        let calls = invoker.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("test goal"));
        assert!(calls[1].contains("I found three matches."));
    }

    #[tokio::test]
    async fn brain_consumes_agent_result_emits_next_dispatch() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"first"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:planner","prompt":"second based on scout"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"all done"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id2 = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id2,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // First dispatch.
        let scout_dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:scout")
        })
        .await
        .expect("first dispatch");

        // Inject scout result.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "scout result",
            Some(scout_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                assistant_text: "Scout findings here.".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        // Second dispatch (planner).
        let planner_dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:planner")
        })
        .await
        .expect("planner dispatch");

        // Inject planner result.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:coordinator",
            "planner result",
            Some(planner_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "planner".to_string(),
                assistant_text: "Plan here.".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");
    }

    #[tokio::test]
    async fn brain_emits_finish_done_terminates_loop() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"trivial goal"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            job_id.clone(),
            "trivial".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "done");
        assert_eq!(result.summary, "trivial goal");

        // JobFinished was emitted.
        let finished = wait_for_envelope_log(&bus, "job_finished", |e| {
            matches!(&e.event, MailboxEvent::JobFinished { job_id: jid, outcome, .. } if jid == &job_id && outcome == "done")
        })
        .await
        .expect("JobFinished emitted");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, .. } => {
                assert_eq!(outcome, "done");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_emits_finish_failed_terminates_loop() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"failed","summary":"reviewer rejected"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-fail".to_string(),
            "trivial".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert_eq!(result.summary, "reviewer rejected");
    }

    #[tokio::test]
    async fn brain_emits_ask_user_terminates_with_ask_user_outcome() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"ask_user","question":"OAuth or API key?"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            job_id.clone(),
            "ambiguous".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "ask_user");
        assert_eq!(result.summary, "OAuth or API key?");

        // JobFinished was emitted with outcome="ask_user".
        let finished = wait_for_envelope_log(&bus, "job_finished", |e| {
            matches!(&e.event, MailboxEvent::JobFinished { job_id: jid, outcome, .. } if jid == &job_id && outcome == "ask_user")
        })
        .await
        .expect("JobFinished ask_user emitted");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, summary, .. } => {
                assert_eq!(outcome, "ask_user");
                assert_eq!(summary, "OAuth or API key?");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_consumes_help_request_emits_help_outcome() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // Turn 1: Dispatch a task with help-loop on.
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:backend-builder","prompt":"build it","with_help_loop":true}"#
                    .to_string(),
            ),
            // Turn 2: AgentHelpRequest comes in; emit help_outcome.
            MockReply::AssistantText(
                r#"{"action":"help_outcome","target":"agent:backend-builder","body_json":"{\"action\":\"direct_answer\",\"answer\":\"User.id\"}"}"#
                    .to_string(),
            ),
            // Turn 3: After help_outcome the brain re-asks; emit finish.
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"helped through"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test goal".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // Wait for the first dispatch.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |e| {
            matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:backend-builder")
        })
        .await
        .expect("first dispatch");

        // Inject AgentHelpRequest from the specialist.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:backend-builder",
            "agent:coordinator",
            "help",
            Some(dispatch.id),
            MailboxEvent::AgentHelpRequest {
                job_id: job_id.clone(),
                agent_id: "backend-builder".to_string(),
                reason: "Plan step ambiguous".to_string(),
                question: "Which struct field carries the user id?".to_string(),
            },
        )
        .await
        .expect("emit help req");

        // Wait for the brain to emit CoordinatorHelpOutcome.
        let outcome_env = wait_for_envelope_log(
            &bus,
            "coordinator_help_outcome",
            |e| {
                matches!(
                    &e.event,
                    MailboxEvent::CoordinatorHelpOutcome { target_agent_id, .. }
                    if target_agent_id == "backend-builder"
                )
            },
        )
        .await
        .expect("CoordinatorHelpOutcome emitted");
        match &outcome_env.event {
            MailboxEvent::CoordinatorHelpOutcome {
                outcome_json,
                target_agent_id,
                ..
            } => {
                assert_eq!(target_agent_id, "backend-builder");
                assert!(outcome_json.contains("direct_answer"));
                assert!(outcome_json.contains("User.id"));
            }
            _ => panic!("unexpected"),
        }

        // Brain finishes after the next finish action — no more
        // events needed because help_outcome doesn't wait for a
        // follow-up event.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");

        // Three turns total — each line in the script consumed.
        let calls = invoker.calls();
        assert_eq!(calls.len(), 3);
        // Turn 2 saw the AgentHelpRequest-shaped prompt.
        assert!(calls[1].contains("blocker"));
        assert!(calls[1].contains("Plan step ambiguous"));
    }

    #[tokio::test]
    async fn brain_max_dispatches_cap_terminates_with_failed() {
        // Script returns dispatches forever — past the cap the
        // brain bails.
        let mut script = Vec::new();
        for _ in 0..10 {
            script.push(MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"loop"}"#
                    .to_string(),
            ));
        }
        let invoker =
            Arc::new(ScriptedCoordinatorInvoker::new(script));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 2).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                2, // max_dispatches=2
            )
            .await
        });

        // Inject results so the brain reaches the next dispatch
        // round each time. We need 2 results for the 2 dispatches
        // before the cap trips on dispatch #3.
        for i in 0..2 {
            let dispatch = wait_for_envelope_log(
                &bus,
                "task_dispatch",
                |e| {
                    if let MailboxEvent::TaskDispatch { .. } = &e.event {
                        // Match by id ordering — the i'th dispatch.
                        true
                    } else {
                        false
                    }
                },
            )
            .await
            .expect("dispatch emitted");

            // Wait until we have at least i+1 dispatches.
            loop {
                let rows = bus
                    .list_typed(Some("task_dispatch"), None, None)
                    .await
                    .expect("list");
                if rows.len() >= i + 1 {
                    break;
                }
                tokio::time::sleep(StdDuration::from_millis(20)).await;
            }
            let _ = dispatch;

            // Pull all dispatches and use the i'th one.
            let dispatches = bus
                .list_typed(Some("task_dispatch"), None, None)
                .await
                .expect("list");
            let parent = dispatches[i].id;

            bus.emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:coordinator",
                "result",
                Some(parent),
                MailboxEvent::AgentResult {
                    job_id: job_id.clone(),
                    agent_id: "scout".to_string(),
                    assistant_text: "result".to_string(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                },
            )
            .await
            .expect("emit");
        }

        let result = timeout(StdDuration::from_secs(10), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("max dispatches"),
            "summary: {}",
            result.summary
        );
    }

    #[tokio::test]
    async fn brain_cancel_mid_loop_terminates_with_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // First turn: emit a dispatch so the brain enters the
            // wait-for-event branch.
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"long"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        // Wait for the first dispatch (so the brain is in
        // wait-for-event).
        wait_for_envelope_log(&bus, "task_dispatch", |_| true)
            .await
            .expect("dispatch emitted");

        // Signal cancel.
        cancel.notify_waiters();

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "failed");
        assert_eq!(result.summary, "cancelled by user");
    }

    #[tokio::test]
    async fn brain_handles_coordinator_session_crash() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::Error(AppError::SwarmInvoke(
                "subprocess died".into(),
            )),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-crash".to_string(),
            "test".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("subprocess died"),
            "summary: {}",
            result.summary
        );
    }

    #[tokio::test]
    async fn brain_resumes_loop_after_help_outcome() {
        // help_outcome must NOT count as a dispatch — verify by
        // running with cap=1 and threading through one help_outcome
        // and one dispatch + finish.
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            // Turn 1: dispatch (counts as 1)
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"go","with_help_loop":true}"#
                    .to_string(),
            ),
            // Turn 2: help_outcome (does NOT count)
            MockReply::AssistantText(
                r#"{"action":"help_outcome","target":"agent:scout","body_json":"{\"action\":\"direct_answer\",\"answer\":\"x\"}"}"#
                    .to_string(),
            ),
            // Turn 3: finish — cap=1 still respected because
            // help_outcome was not counted.
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 1).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                1,
            )
            .await
        });

        // Wait for the first dispatch and inject AgentHelpRequest.
        let dispatch = wait_for_envelope_log(&bus, "task_dispatch", |_| true)
            .await
            .expect("dispatch emitted");
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:scout",
            "agent:coordinator",
            "help",
            Some(dispatch.id),
            MailboxEvent::AgentHelpRequest {
                job_id: job_id.clone(),
                agent_id: "scout".to_string(),
                reason: "blocked".to_string(),
                question: "?".to_string(),
            },
        )
        .await
        .expect("emit help");

        // Brain emits help_outcome, then re-invokes (no event needed)
        // and emits finish.
        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");

        // Verify the cap was NOT tripped: 1 dispatch only.
        let dispatches = bus
            .list_typed(Some("task_dispatch"), None, None)
            .await
            .expect("list");
        assert_eq!(dispatches.len(), 1);
        // help_outcome was emitted.
        let help_outcomes = bus
            .list_typed(Some("coordinator_help_outcome"), None, None)
            .await
            .expect("list");
        assert_eq!(help_outcomes.len(), 1);
    }

    #[tokio::test]
    async fn brain_emits_dispatch_with_correct_parent_id_chain() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:scout","prompt":"first"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"dispatch","target":"agent:planner","prompt":"second"}"#
                    .to_string(),
            ),
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"done","summary":"ok"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let app_for_brain = app.handle().clone();
        let bus_for_brain = Arc::clone(&bus);
        let cancel_for_brain = Arc::clone(&cancel);
        let invoker_for_brain = Arc::clone(&invoker);
        let job_id_for_brain = job_id.clone();
        let brain_handle = tokio::spawn(async move {
            CoordinatorBrain::run_with_max(
                app_for_brain,
                ws,
                job_id_for_brain,
                "test".to_string(),
                invoker_for_brain,
                bus_for_brain,
                cancel_for_brain,
                30,
            )
            .await
        });

        let scout_dispatch =
            wait_for_envelope_log(&bus, "task_dispatch", |e| {
                matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:scout")
            })
            .await
            .expect("scout dispatch");

        // Inject scout result.
        let scout_result_env = bus
            .emit_typed(
                app.handle(),
                "default",
                "agent:scout",
                "agent:coordinator",
                "result",
                Some(scout_dispatch.id),
                MailboxEvent::AgentResult {
                    job_id: job_id.clone(),
                    agent_id: "scout".to_string(),
                    assistant_text: "found".to_string(),
                    total_cost_usd: 0.01,
                    turn_count: 1,
                },
            )
            .await
            .expect("emit");

        // The next dispatch should chain its parent_id to the
        // scout-result envelope's id.
        let planner_dispatch =
            wait_for_envelope_log(&bus, "task_dispatch", |e| {
                matches!(&e.event, MailboxEvent::TaskDispatch { target, .. } if target == "agent:planner")
            })
            .await
            .expect("planner dispatch");
        assert_eq!(planner_dispatch.parent_id, Some(scout_result_env.id));

        // Inject planner result so brain finishes.
        bus.emit_typed(
            app.handle(),
            "default",
            "agent:planner",
            "agent:coordinator",
            "result",
            Some(planner_dispatch.id),
            MailboxEvent::AgentResult {
                job_id: job_id.clone(),
                agent_id: "planner".to_string(),
                assistant_text: "plan".to_string(),
                total_cost_usd: 0.01,
                turn_count: 1,
            },
        )
        .await
        .expect("emit");

        let result = timeout(StdDuration::from_secs(5), brain_handle)
            .await
            .expect("brain timeout")
            .expect("join")
            .expect("brain ok");
        assert_eq!(result.outcome, "done");
    }

    #[tokio::test]
    async fn brain_finish_outcome_other_than_done_or_failed_normalised_to_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText(
                r#"{"action":"finish","outcome":"weird","summary":"strange"}"#
                    .to_string(),
            ),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-norm".to_string(),
            "test".to_string(),
            invoker,
            Arc::clone(&bus),
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed", "outcome normalised to failed");
        assert_eq!(result.summary, "strange");

        // The emitted JobFinished also reflects the normalised outcome.
        let finished = wait_for_envelope_log(&bus, "job_finished", |_| true)
            .await
            .expect("JobFinished");
        match &finished.event {
            MailboxEvent::JobFinished { outcome, .. } => {
                assert_eq!(outcome, "failed");
            }
            _ => panic!("unexpected"),
        }
    }

    #[tokio::test]
    async fn brain_handles_parse_error_as_failed() {
        let invoker = Arc::new(ScriptedCoordinatorInvoker::new(vec![
            MockReply::AssistantText("Just garbage no JSON.".to_string()),
        ]));
        let (app, bus, cancel, ws, _job_id, _dir) =
            setup_brain(Arc::clone(&invoker), 30).await;

        let result = CoordinatorBrain::run_with_max(
            app.handle().clone(),
            ws,
            "j-parse".to_string(),
            "test".to_string(),
            invoker,
            bus,
            cancel,
            30,
        )
        .await
        .expect("ok");
        assert_eq!(result.outcome, "failed");
        assert!(
            result.summary.contains("parse error"),
            "summary: {}",
            result.summary
        );
    }

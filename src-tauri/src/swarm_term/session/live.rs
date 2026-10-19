//! The opt-in LIVE harness: the real session — real claude panes with the
//! production personas, permissions and bridge — driven headless from a
//! test. [`run_live`] is one run: a task pasted to the orchestrator as the
//! chat bar does, the swarm watched until it settles, its changes,
//! transcripts and denied tool calls collected, the session stopped.
//! `tests::live_swarm_run` drives it from `NEURON_LIVE_*`; the H3
//! reference tasks (`swarm_term::reference`) run on it too. Spends tokens.

use super::*;

use std::sync::atomic::AtomicUsize;

use tauri::Listener;

use crate::swarm_term::checkpoint::ChangeKind;
use crate::swarm_term::lifecycle;

/// Fault injection: extra system-prompt text per agent (e.g. "reject every
/// review"), so failure paths can be driven with REAL agents. Test builds
/// only — `start()` appends it to the persona; the app never has it.
static INJECTIONS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

pub(super) fn with_injection(agent_id: &str, prompt: String) -> String {
    let g = INJECTIONS.lock().unwrap_or_else(|p| p.into_inner());
    g.iter()
        .filter(|(agent, _)| agent == agent_id)
        .fold(prompt, |p, (_, extra)| {
            format!("{p}\n\n## TEST OTURUMU TALİMATI (geçerli, uygula)\n\n{extra}")
        })
}

/// One live run.
pub(crate) struct LiveSpec {
    pub project: PathBuf,
    /// Pasted to the orchestrator.
    pub task: String,
    /// Where the transcripts are copied before `stop()` deletes them.
    pub out: PathBuf,
    pub timeout: Duration,
    /// Terminal signals (TASK_DONE / TASK_FAILED) the orchestrator must
    /// receive before the run may wind down.
    pub expect: usize,
    /// How long the swarm must then be quiet — no new message, no task
    /// open in the lifecycle — to end the run: the orchestrator writes its
    /// summary meanwhile.
    pub settle: Duration,
    pub inject: Vec<(String, String)>,
    /// Saved as Settings → Models does before the start.
    pub team: Option<team::SwarmTermTeam>,
    /// Append the orchestrator pane's raw output stream here.
    pub raw_dump: Option<PathBuf>,
    /// Print the orchestrator's screen this long after the start.
    pub screen_at: Option<Duration>,
    /// Kill this agent's claude before the task and restart it.
    pub restart: Option<String>,
    /// Revert every change at the end.
    pub revert: bool,
}

impl LiveSpec {
    /// `NEURON_LIVE_PROJECT` (project dir), `NEURON_LIVE_TASK`,
    /// `NEURON_LIVE_OUT` (transcript copies), `NEURON_LIVE_TIMEOUT_MIN`
    /// (default 20), `NEURON_LIVE_EXPECT` (default 2: a build task + the
    /// smoke run), `NEURON_LIVE_INJECT` (JSON `{"<agent>": "<extra persona
    /// text>"}`), `NEURON_LIVE_TEAM` (a `SwarmTermTeam` JSON),
    /// `NEURON_LIVE_RAW_DUMP=<file>`, `NEURON_LIVE_SCREEN_AT=<s>`,
    /// `NEURON_LIVE_RESTART=<agent>`, `NEURON_LIVE_REVERT=1`.
    pub(crate) fn from_env() -> Self {
        let required = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            project: PathBuf::from(required("NEURON_LIVE_PROJECT")),
            task: required("NEURON_LIVE_TASK"),
            out: PathBuf::from(required("NEURON_LIVE_OUT")),
            timeout: Duration::from_secs(
                60 * var("NEURON_LIVE_TIMEOUT_MIN").and_then(|v| v.parse().ok()).unwrap_or(20u64),
            ),
            expect: var("NEURON_LIVE_EXPECT").and_then(|v| v.parse().ok()).unwrap_or(2),
            settle: Duration::from_secs(60),
            inject: var("NEURON_LIVE_INJECT")
                .map(|raw| {
                    serde_json::from_str::<HashMap<String, String>>(&raw)
                        .expect("NEURON_LIVE_INJECT must be a JSON object")
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            team: var("NEURON_LIVE_TEAM").map(|raw| {
                serde_json::from_str(&raw).expect("NEURON_LIVE_TEAM must be a SwarmTermTeam")
            }),
            raw_dump: var("NEURON_LIVE_RAW_DUMP").map(PathBuf::from),
            screen_at: var("NEURON_LIVE_SCREEN_AT")
                .and_then(|v| v.parse::<u64>().ok())
                .map(Duration::from_secs),
            restart: var("NEURON_LIVE_RESTART"),
            revert: std::env::var("NEURON_LIVE_REVERT").is_ok(),
        }
    }
}

/// A TASK_DONE / TASK_FAILED the orchestrator received.
#[derive(Debug, Clone)]
pub(crate) struct TaskSignal {
    /// Since the run started.
    pub at: Duration,
    pub done: bool,
    pub task_id: Option<String>,
}

/// What one live run did. Durations count from the start of the run.
#[derive(Debug, Default)]
pub(crate) struct LiveReport {
    pub panes: usize,
    /// Panes that answered their kickoff when the readiness wait ended.
    pub ready: usize,
    pub ready_at: Duration,
    pub not_ready: Vec<String>,
    pub task_sent_at: Duration,
    pub signals: Vec<TaskSignal>,
    /// When the watch ended, and whether the timeout ended it.
    pub ended_at: Duration,
    /// The agents' last transcript write: when the swarm last did
    /// anything, signal or not (a refusal ends without one).
    pub last_activity_at: Option<Duration>,
    pub timed_out: bool,
    /// Envelopes delivered (`processed/`) and refused (`rejected/`).
    pub delivered: usize,
    pub rejected: usize,
    /// `[Neuron]` notices and stall reminders the bridge pasted.
    pub notices: usize,
    pub reminders: usize,
    /// Times the message budget held the swarm (the harness raises it).
    pub budget_holds: usize,
    /// What the Changes panel shows of the run.
    pub changed: Vec<(String, ChangeKind)>,
    pub claude_version: Option<String>,
    pub lite_team: bool,
    /// Agent → its transcript's copy in [`LiveSpec::out`].
    pub transcripts: BTreeMap<String, PathBuf>,
    pub orchestrator_final: Option<String>,
    /// Denied tool calls: (agent, the start of the error).
    pub denied: Vec<(String, String)>,
    /// Session invariants the run broke (empty when all held).
    pub broken: Vec<String>,
}

/// One live run of `spec`; prints the timeline as it goes.
pub(crate) async fn run_live(spec: &LiveSpec) -> LiveReport {
    let started = Instant::now();
    // The same moment on the wall clock, which file times are read on.
    let started_wall = std::time::SystemTime::now();
    let stamp = |t: Instant| format!("{:>5.1}s", t.duration_since(started).as_secs_f32());
    let mut report = LiveReport::default();

    {
        let mut g = INJECTIONS.lock().unwrap_or_else(|p| p.into_inner());
        g.clear();
        for (agent, extra) in &spec.inject {
            println!("INJECT [{agent}]: {extra}");
            g.push((agent.clone(), extra.clone()));
        }
    }

    let (app, pool, _db) = crate::test_support::mock_app_with_pool_and_terminal_registry().await;
    let registry = app.state::<TerminalRegistry>().inner().clone();
    // The operator picked the folder: the user's "I trust it" in the UI.
    trust::trust(&pool, &spec.project).await.expect("trust the project");
    // Bridge-originated pastes (notices, reminders) and lifecycle
    // transitions never touch the filesystem queue — print them too.
    let notices = Arc::new(AtomicUsize::new(0));
    let reminders = Arc::new(AtomicUsize::new(0));
    {
        let (notices, reminders) = (Arc::clone(&notices), Arc::clone(&reminders));
        app.listen_any(crate::events::SWARM_TERM_ROUTE, move |e| {
            let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
            let outcome = v["outcome"].as_str().unwrap_or("");
            let counter = match outcome {
                "notice" => &notices,
                "reminder" => &reminders,
                _ => return,
            };
            counter.fetch_add(1, Ordering::SeqCst);
            let body: String = v["body"].as_str().unwrap_or("").chars().take(220).collect();
            println!(
                "[{:>5.1}s] {} → {}: {}",
                started.elapsed().as_secs_f32(),
                outcome.to_uppercase(),
                v["target"].as_str().unwrap_or("?"),
                body.replace('\n', " ⏎ ")
            );
        });
        app.listen_any(crate::events::SWARM_TERM_LIFECYCLE, move |e| {
            let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
            println!(
                "[{:>5.1}s] LIFECYCLE {} [{}] {} → {} owners={}",
                started.elapsed().as_secs_f32(),
                v["source"].as_str().unwrap_or("?"),
                v["task_id"].as_str().unwrap_or("?"),
                v["transition"].as_str().unwrap_or("?"),
                v["state"].as_str().unwrap_or("?"),
                v["owners"],
            );
        });
        app.listen_any(crate::events::SWARM_TERM_PANE_STATE, move |e| {
            let v: serde_json::Value = serde_json::from_str(e.payload()).unwrap_or_default();
            println!(
                "[{:>5.1}s] PANE {} → {}{}",
                started.elapsed().as_secs_f32(),
                v["agentId"].as_str().unwrap_or("?"),
                v["readiness"].as_str().unwrap_or("?"),
                v["detail"].as_str().map(|d| format!(" ({d})")).unwrap_or_default(),
            );
        });
    }
    if let Some(settings) = &spec.team {
        println!("TEAM: {settings:?}");
        team::save(&pool, settings).await.expect("save the team settings");
    }
    let swarm = Arc::new(TerminalSwarmRegistry::new());
    swarm.start(app.handle().clone(), spec.project.clone()).await.expect("start");
    let (panes, ready, root, claude_sessions, lifecycle_store) = {
        let g = swarm.inner.lock().unwrap();
        let s = g.as_ref().expect("session");
        let panes = s.panes.read().unwrap().clone();
        (
            panes,
            Arc::clone(&s.ready_panes),
            s.bridgespace_root.clone().unwrap(),
            PaneManifest::read(s.session_dir.as_deref().unwrap())
                .expect("pane manifest")
                .claude_sessions,
            Arc::clone(&s.lifecycle),
        )
    };
    report.panes = panes.len();
    if let Some(h) = swarm.current() {
        report.claude_version = h.claude_version;
        report.lite_team = h.lite_team;
    }
    println!("[{}] session started: {}", stamp(Instant::now()), root.display());
    if let (Some(dump), Some(pane)) = (spec.raw_dump.clone(), panes.get("orchestrator")) {
        use std::io::Write as _;
        app.listen_any(crate::events::pane_data(pane), move |e| {
            let text: String = serde_json::from_str(e.payload()).unwrap_or_default();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&dump) {
                let _ = write!(f, "{text}");
            }
        });
    }

    // Readiness (the session answers claude's folder-trust screen itself).
    let mut screen_shown = false;
    let ready_deadline = Instant::now() + Duration::from_secs(150);
    loop {
        let n = ready.lock().map(|g| g.len()).unwrap_or(0);
        if n == panes.len() || Instant::now() > ready_deadline {
            println!("[{}] ready panes: {n}/{}", stamp(Instant::now()), panes.len());
            report.ready = n;
            report.ready_at = started.elapsed();
            // What the UI is shown must agree with the delivery gate.
            let states = swarm.current().expect("session").pane_states;
            let shown_ready = states.iter().filter(|s| s.readiness == PaneReadiness::Ready).count();
            if shown_ready != n {
                report.broken.push(format!("pane states disagree with the ready set: {states:?}"));
            }
            break;
        }
        if !screen_shown && spec.screen_at.is_some_and(|d| started.elapsed() >= d) {
            screen_shown = true;
            if let Some(pane) = panes.get("orchestrator") {
                let screen = pane_screen_tail(&registry, &pool, pane).await.unwrap_or_default();
                let n = screen.chars().count();
                let tail: String = screen.chars().skip(n.saturating_sub(1500)).collect();
                println!(
                    "SCREEN [orchestrator] at {} ({n} chars, blocking={}):\n{tail}",
                    stamp(Instant::now()),
                    screen_is_blocking(&screen)
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Diagnose panes that never became ready: their screen tail.
    let ready_now = ready.lock().map(|g| g.clone()).unwrap_or_default();
    for (agent, pane) in &panes {
        if ready_now.contains(pane) {
            continue;
        }
        report.not_ready.push(agent.clone());
        let screen = pane_screen_tail(&registry, &pool, pane).await.unwrap_or_default();
        let n = screen.chars().count();
        let tail: String = screen.chars().skip(n.saturating_sub(700)).collect();
        println!(
            "NOT READY [{agent}] ({n} chars, blocking={}) screen:\n{tail}",
            screen_is_blocking(&screen)
        );
    }

    // A crash: the agent's claude is killed, shown as exited, and restarted
    // on the same conversation before the task goes out.
    let panes = match &spec.restart {
        Some(agent) => {
            let old = panes.get(agent.as_str()).expect("pane of the agent to restart").clone();
            let shown = |r: PaneReadiness| {
                swarm.current().is_some_and(|h| {
                    h.pane_states.iter().any(|s| s.agent_id == *agent && s.readiness == r)
                })
            };
            let _ = registry.kill_pane(&old, &pool).await;
            let t = Instant::now();
            while !shown(PaneReadiness::Exited) && t.elapsed() < Duration::from_secs(20) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            println!(
                "[{}] {agent} killed — shown exited: {}",
                stamp(Instant::now()),
                shown(PaneReadiness::Exited)
            );
            assert!(shown(PaneReadiness::Exited), "a killed pane must show as exited");
            let handle = swarm.restart_agent(app.handle().clone(), agent).await.expect("restart");
            let new = handle.panes.iter().find(|p| p.agent_id == *agent).expect("pane").pane_id.clone();
            assert_ne!(new, old, "a restart runs in a new pane");
            let t = Instant::now();
            while !ready.lock().unwrap().contains(&new) && t.elapsed() < Duration::from_secs(150) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let back = ready.lock().unwrap().contains(&new);
            println!(
                "[{}] {agent} restarted — ready again after {:.1}s: {back}",
                stamp(Instant::now()),
                t.elapsed().as_secs_f32()
            );
            assert!(back, "the restarted agent must answer READY again");
            assert!(shown(PaneReadiness::Ready));
            let g = swarm.inner.lock().unwrap();
            let s = g.as_ref().expect("session");
            let panes = s.panes.read().unwrap().clone();
            panes
        }
        None => panes,
    };

    // Task → orchestrator, exactly like the chat bar.
    let orch = panes.get("orchestrator").expect("orchestrator pane");
    let paste = format!("\x1b[200~{}\x1b[201~", bridge::sanitize_for_paste(&spec.task));
    bridge::paste_then_submit(&registry, orch, paste.as_bytes()).await.expect("task");
    let task_sent = Instant::now();
    report.task_sent_at = started.elapsed();
    println!("[{}] task sent to orchestrator", stamp(task_sent));

    // Timeline: every file that lands in processed/ or rejected/.
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    // Task ids the envelopes carried; the lifecycle says which are open.
    let mut task_ids: BTreeSet<String> = BTreeSet::new();
    let mut last_message = task_sent;
    // The message budget holding the swarm, and the user raising it.
    let mut held_since: Option<Instant> = None;
    report.timed_out = true;
    while started.elapsed() < spec.timeout {
        for kind in ["processed", "rejected"] {
            for target in AGENT_IDS {
                let dir = root.join(kind).join(target);
                let Ok(rd) = std::fs::read_dir(&dir) else { continue };
                let mut files: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                    .collect();
                files.sort();
                for f in files {
                    if !seen.insert(f.clone()) {
                        continue;
                    }
                    last_message = Instant::now();
                    let raw = std::fs::read_to_string(&f).unwrap_or_default();
                    let v: serde_json::Value =
                        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                    let full_body = v["body"].as_str().unwrap_or("");
                    let task_id = v["task_id"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| v["task_id"].as_u64().map(|n| n.to_string()))
                        .or_else(|| lifecycle::body_task_id(full_body).map(str::to_string));
                    let body: String = full_body.replace('\n', " ⏎ ").chars().take(170).collect();
                    let from = v["from"].as_str().unwrap_or("?");
                    let reason = if kind == "rejected" {
                        let mut r = f.clone();
                        r.set_extension("reason");
                        format!(" REJECTED: {}", std::fs::read_to_string(r).unwrap_or_default())
                    } else {
                        String::new()
                    };
                    println!(
                        "[{}] {from} → {target} [{}]: {body}{reason}",
                        stamp(Instant::now()),
                        v["task_id"].as_str().map(str::to_string).unwrap_or_else(|| v["task_id"].to_string()),
                    );
                    if kind == "rejected" {
                        report.rejected += 1;
                        continue;
                    }
                    report.delivered += 1;
                    if let Some(id) = &task_id {
                        task_ids.insert(id.clone());
                    }
                    let head = full_body.trim_start();
                    let done = head.starts_with("TASK_DONE");
                    if *target == "orchestrator" && (done || head.starts_with("TASK_FAILED")) {
                        report.signals.push(TaskSignal {
                            at: started.elapsed(),
                            done,
                            task_id: signal_task_id(head).or(task_id),
                        });
                    }
                }
            }
        }
        let handle = swarm.current().expect("session");
        match (handle.budget_reached, held_since) {
            (true, None) => {
                report.budget_holds += 1;
                held_since = Some(Instant::now());
                println!(
                    "[{}] BUDGET reached: {}/{} delivered — the swarm holds",
                    stamp(Instant::now()),
                    handle.messages_delivered,
                    handle.message_budget
                );
            }
            (true, Some(t)) if t.elapsed() > Duration::from_secs(3) => {
                let after = swarm.extend_budget(app.handle()).expect("extend the budget");
                held_since = None;
                println!(
                    "[{}] BUDGET raised to {} — the swarm runs again",
                    stamp(Instant::now()),
                    after.message_budget
                );
                assert!(!after.budget_reached);
            }
            _ => {}
        }
        // Every expected terminal signal arrived, no task the lifecycle
        // tracks is still open (a smoke run may follow a build), and the
        // swarm has been quiet a while: the orchestrator has written its
        // final summary.
        let open = task_ids
            .iter()
            .any(|id| lifecycle_store.closure(id).is_none() && !lifecycle_store.holders(id).is_empty());
        if report.signals.len() >= spec.expect && !open && last_message.elapsed() > spec.settle {
            report.timed_out = false;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    report.ended_at = started.elapsed();
    report.notices = notices.load(Ordering::SeqCst);
    report.reminders = reminders.load(Ordering::SeqCst);
    println!(
        "[{}] TASK_DONE/TASK_FAILED received by orchestrator: {}/{}{}",
        stamp(Instant::now()),
        report.signals.len(),
        spec.expect,
        if report.timed_out { " (timed out)" } else { "" }
    );
    if report.budget_holds > 0 {
        println!("BUDGET holds during the run: {}", report.budget_holds);
    }

    // What the Changes panel shows of the run (see `checkpoint`); a revert
    // puts all of it back.
    let app_data = app.path().app_data_dir().expect("app data dir");
    let changed = checkpoint::changes(&app_data, &spec.project);
    report.changed = changed.files.iter().map(|f| (f.path.clone(), f.kind)).collect();
    println!(
        "CHANGES since the checkpoint: {:?}{}",
        changed.files.iter().map(|f| (f.path.as_str(), f.kind)).collect::<Vec<_>>(),
        changed.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default()
    );
    if spec.revert {
        let left = checkpoint::revert(&app_data, &spec.project, None).expect("revert");
        println!("REVERTED — left changed: {:?}", left.files);
        if !left.files.is_empty() {
            report.broken.push(format!("a full revert left changes: {:?}", left.files));
        }
    }

    // Denied tool calls + transcript copies, before stop() deletes the
    // transcripts.
    let projects_dirs = pane_config::claude_projects_dirs();
    std::fs::create_dir_all(&spec.out).ok();
    // The folders claude keeps this project's transcripts in.
    let mut transcript_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for (agent, claude_session) in &claude_sessions {
        let Some(file) = pane_config::find_transcript(&projects_dirs, claude_session) else {
            println!("NO TRANSCRIPT [{agent}]");
            continue;
        };
        if let Some(dir) = file.parent().filter(|d| !projects_dirs.iter().any(|root| root == d)) {
            transcript_dirs.insert(dir.to_path_buf());
        }
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        // claude appends to the transcript as it works: its last write
        // is the agent's last activity.
        let written = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(started_wall).ok());
        report.last_activity_at = report.last_activity_at.max(written);
        let copy = spec.out.join(format!("{agent}.jsonl"));
        if std::fs::write(&copy, &text).is_ok() {
            report.transcripts.insert(agent.clone(), copy);
        }
        // The orchestrator's last words are the run's summary.
        if agent == "orchestrator" {
            let last = text
                .lines()
                .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .filter(|v| v["type"] == "assistant")
                .flat_map(|v| {
                    v.pointer("/message/content").and_then(|c| c.as_array().cloned()).unwrap_or_default()
                })
                .filter_map(|p| p["text"].as_str().map(str::to_string))
                .next_back();
            if let Some(last) = &last {
                println!("ORCHESTRATOR FINAL:\n{last}");
            }
            report.orchestrator_final = last;
        }
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let Some(parts) = v.pointer("/message/content").and_then(|c| c.as_array()) else {
                continue;
            };
            for part in parts {
                if part["type"] == "tool_result" && part["is_error"] == true {
                    let c = part["content"].to_string();
                    if c.to_lowercase().contains("permission") || c.contains("denied") {
                        let c: String = c.chars().take(200).collect();
                        println!("DENIED [{agent}] {c}");
                        report.denied.push((agent.clone(), c));
                    }
                }
            }
        }
    }

    swarm.stop(app.handle().clone()).await.expect("stop");
    println!("[{}] session stopped", stamp(Instant::now()));
    let left: Vec<&str> = claude_sessions
        .iter()
        .filter(|(_, id)| pane_config::find_transcript(&projects_dirs, id).is_some())
        .map(|(agent, _)| agent.as_str())
        .collect();
    if !left.is_empty() {
        report.broken.push(format!("transcripts left behind: {left:?}"));
    }
    // A run on a fresh folder leaves claude's folder for it empty once its
    // transcripts are gone: removed too. `remove_dir` takes only an empty
    // folder — anything else claude or the user keeps there stays.
    for dir in &transcript_dirs {
        if std::fs::remove_dir(dir).is_err() && dir.exists() {
            println!("KEPT claude's project folder (not empty): {}", dir.display());
        }
    }
    INJECTIONS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    report
}

/// Pure: the task id of a `TASK_DONE <id> …` / `TASK_FAILED <id> …` body.
fn signal_task_id(body: &str) -> Option<String> {
    let id = body
        .split_whitespace()
        .nth(1)?
        .trim_matches(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'));
    (!id.is_empty()).then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_names_its_task() {
        assert_eq!(signal_task_id("TASK_DONE T1 — approved").as_deref(), Some("T1"));
        assert_eq!(signal_task_id("TASK_FAILED `S-2`: tests fail").as_deref(), Some("S-2"));
        assert_eq!(signal_task_id("TASK_DONE"), None);
        assert_eq!(signal_task_id("TASK_DONE —"), None);
    }
}

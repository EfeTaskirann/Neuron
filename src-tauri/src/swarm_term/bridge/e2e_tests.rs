//! Opt-in end-to-end test of the bridge against REAL PTY panes.
//!
//! Each of the 9 agents is a real pane running a tiny fake agent (a
//! python script that appends every character it receives on stdin to
//! a per-agent log file). Envelopes are dropped into the inbox exactly
//! as a claude `Write` call would, the real watcher delivers them, and
//! the test asserts on the filesystem queue + what each pane received.
//!
//! Run with:
//!   cargo test --lib swarm_term::bridge::e2e_tests -- --ignored --nocapture
//! `NEURON_E2E_PYTHON` overrides the interpreter (default `python`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use tauri::Manager;

use crate::models::PaneSpawnInput;
use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::AGENT_IDS;
use crate::swarm_term::lifecycle::LifecycleStore;
use crate::test_support::mock_app_with_pool_and_terminal_registry;

use super::{install, prepare_layout, uninstall};

/// Fake agent: optional banner line (argv[2]) on stdout, then append
/// every received character to argv[1]. `msvcrt.getwch` reads the
/// console char-by-char (no line buffering, no echo) on Windows.
const FAKE_AGENT: &str = r#"
import sys
log = sys.argv[1]
if len(sys.argv) > 2:
    print(sys.argv[2], flush=True)
out = open(log, 'a', encoding='utf-8', newline='')
try:
    import msvcrt
    read = msvcrt.getwch
except ImportError:
    read = lambda: sys.stdin.read(1)
while True:
    c = read()
    if c == '':
        break
    out.write(c)
    out.flush()
"#;

fn fwd(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// Atomic inbox drop (`.tmp` + rename), optionally back-dating the
/// file so time-based retry expiry can be exercised without waiting.
fn drop_raw(root: &Path, target: &str, name: &str, content: &str, age: Option<Duration>) {
    let dir = root.join("inbox").join(target);
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, content).expect("write tmp");
    if let Some(age) = age {
        let f = fs::File::options().write(true).open(&tmp).expect("open tmp");
        f.set_modified(SystemTime::now() - age).expect("set mtime");
    }
    fs::rename(&tmp, dir.join(format!("{name}.json"))).expect("rename");
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns 9 real PTY panes running python — opt in via --ignored"]
async fn bridge_end_to_end_with_fake_agents() {
    let (app, pool, _db_dir) = mock_app_with_pool_and_terminal_registry().await;
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let work = tempfile::tempdir().expect("tempdir");
    let script = work.path().join("fake_agent.py");
    fs::write(&script, FAKE_AGENT).expect("script");
    let python =
        std::env::var("NEURON_E2E_PYTHON").unwrap_or_else(|_| "python".into());

    // ---- 9 fake-agent panes ----------------------------------------- //
    let mut panes_by_agent: HashMap<String, String> = HashMap::new();
    let mut logs: HashMap<&str, PathBuf> = HashMap::new();
    for &agent in AGENT_IDS {
        let log = work.path().join(format!("{agent}.log"));
        fs::write(&log, "").expect("log");
        // integration-tester runs as `claude-code` and prints a line the
        // approval regex matches, so its pane is stuck in
        // `awaiting_approval` (the H4 scenario).
        let (kind, banner) = if agent == "integration-tester" {
            ("claude-code", " 'Approve this change?'")
        } else {
            ("shell", "")
        };
        let cmd = format!("{python} '{}' '{}'{banner}", fwd(&script), fwd(&log));
        let pane = registry
            .spawn_pane(
                PaneSpawnInput {
                    cwd: fwd(work.path()),
                    cmd: Some(cmd),
                    cols: Some(220),
                    rows: Some(50),
                    agent_kind: Some(kind.into()),
                    role: None,
                    workspace: None,
                    extra_env: None,
                },
                app.handle().clone(),
                pool.clone(),
            )
            .await
            .expect("spawn fake agent");
        panes_by_agent.insert(agent.to_string(), pane.id);
        logs.insert(agent, log);
    }
    let log = |a: &str| read(&logs[a]);

    // Let the interpreters boot, and wait for the approval false
    // positive to land on the tester pane.
    let tester_pane = panes_by_agent["integration-tester"].clone();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut tester_status = None;
    while Instant::now() < deadline {
        tester_status = registry.pane_status(&tester_pane).await;
        if tester_status == Some("awaiting_approval") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    println!("integration-tester pane status: {tester_status:?}");
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // ---- Bridge ------------------------------------------------------ //
    let root = work.path().join("bridge");
    prepare_layout(&root).expect("layout");
    // Every pane is ready except planner (the not-ready scenarios).
    let ready: HashSet<String> = panes_by_agent
        .iter()
        .filter(|(a, _)| a.as_str() != "planner")
        .map(|(_, p)| p.clone())
        .collect();
    // FIFO by write time, not by name: dropped BEFORE the watcher
    // starts so both are in the same first scan. The name-wise first
    // file is the newer one.
    drop_raw(&root, "scout", "aa-newer",
        r#"{"from":"coordinator","to":"scout","body":"order-second"}"#, None);
    drop_raw(&root, "scout", "zz-older",
        r#"{"from":"coordinator","to":"scout","body":"order-first"}"#,
        Some(Duration::from_secs(20)));
    let handle = install(
        app.handle().clone(),
        root.clone(),
        panes_by_agent.clone(),
        Arc::new(Mutex::new(ready)),
        Arc::new(LifecycleStore::new()),
    );

    // H1: hierarchy denial → sender must be told.
    drop_raw(&root, "planner", "01",
        r#"{"from":"frontend-reviewer","to":"planner","body":"plan-review-01"}"#, None);
    // H2: reply edges added in wave 2 — reviewer and tester can answer
    // the orchestrator, scout can answer a builder.
    drop_raw(&root, "orchestrator", "09",
        r#"{"from":"backend-reviewer","to":"orchestrator","body":"reply-09"}"#, None);
    drop_raw(&root, "frontend-builder", "10",
        r#"{"from":"scout","to":"frontend-builder","body":"reply-10"}"#, None);
    // H5: capitalised sender, numeric task_id, raw newline in body.
    drop_raw(&root, "backend-builder", "02",
        "{\"from\":\"Coordinator\",\"to\":\"backend-builder\",\"body\":\"satir-bir\nsatir-iki\",\"task_id\":42}", None);
    // Structurally invalid (no body) → rejected after retries, sender told.
    drop_raw(&root, "coordinator", "03",
        r#"{"from":"scout","to":"coordinator"}"#, None);
    // H3 + H7: decorated DONE with a note → review fanout carrying it.
    drop_raw(&root, "coordinator", "04",
        r#"{"from":"backend-builder","to":"coordinator","body":"**DONE** T4 — auth.rs: refresh eklendi","task_id":"T4"}"#, None);
    // H3: "tamam —" completion with a task_id → sender told it was ignored.
    drop_raw(&root, "coordinator", "05",
        r#"{"from":"frontend-builder","to":"coordinator","body":"tamam — login sayfasi bitti","task_id":"T5"}"#, None);
    // H4: target pane stuck in awaiting_approval must still receive.
    drop_raw(&root, "integration-tester", "06",
        r#"{"from":"coordinator","to":"integration-tester","body":"smoke-run-06"}"#, None);
    // H6: not-ready target, message already older than the grace → rejected + sender told.
    drop_raw(&root, "planner", "07",
        r#"{"from":"coordinator","to":"planner","body":"plan-07"}"#,
        Some(Duration::from_secs(120)));
    // H6: not-ready target, fresh message → must stay queued (old code dropped it after ~1 s).
    drop_raw(&root, "planner", "08",
        r#"{"from":"coordinator","to":"planner","body":"plan-08"}"#, None);

    // ---- Assertions (polled) ----------------------------------------- //
    let r = |p: &str| root.join(p);
    type Check<'a> = (&'a str, Box<dyn Fn() -> bool + 'a>);
    let checks: Vec<Check<'_>> = vec![
        ("01 denied → rejected/planner", Box::new(|| r("rejected/planner/01.json").is_file())),
        ("01 .reason names hierarchy", Box::new(|| read(&r("rejected/planner/01.reason")).contains("hierarchy denies"))),
        ("01 sender frontend-reviewer got [Neuron] notice", Box::new(|| { let l = log("frontend-reviewer"); l.contains("[Neuron]") && l.contains("hierarchy denies") })),
        ("09 reviewer→orchestrator reply delivered", Box::new(|| log("orchestrator").contains("reply-09"))),
        ("10 scout→builder reply delivered", Box::new(|| log("frontend-builder").contains("reply-10"))),
        ("02 lenient envelope delivered", Box::new(|| r("processed/backend-builder/02.json").is_file())),
        ("02 builder received both lines from @coordinator", Box::new(|| { let l = log("backend-builder"); l.contains("satir-bir") && l.contains("satir-iki") && l.contains("from @coordinator") })),
        ("03 invalid envelope → rejected/coordinator", Box::new(|| r("rejected/coordinator/03.json").is_file())),
        ("03 sender scout got [Neuron] notice", Box::new(|| { let l = log("scout"); l.contains("[Neuron]") && l.contains("missing field `body`") })),
        ("04 DONE delivered to coordinator", Box::new(|| log("coordinator").contains("**DONE** T4"))),
        // The fake agent's console input drops `—` (python msvcrt), so the
        // separator is not asserted — only that the note follows the id.
        ("04 reviewer got review WITH note", Box::new(|| { let l = log("backend-reviewer"); l.contains("review T4") && l.contains("auth.rs: refresh eklendi") })),
        ("05 frontend-builder told signal not recognised", Box::new(|| log("frontend-builder").contains("TANINMADI"))),
        ("06 awaiting_approval pane still received", Box::new(|| log("integration-tester").contains("smoke-run-06"))),
        ("07 expired not-ready → rejected/planner", Box::new(|| r("rejected/planner/07.json").is_file())),
        ("07 sender coordinator got [Neuron] notice", Box::new(|| log("coordinator").contains("never became ready"))),
        ("FIFO by mtime: older file delivered first", Box::new(|| {
            let l = log("scout");
            matches!((l.find("order-first"), l.find("order-second")), (Some(a), Some(b)) if a < b)
        })),
        ("planner received nothing", Box::new(|| !log("planner").contains("plan-0"))),
    ];
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut failing: Vec<&str>;
    loop {
        failing = checks.iter().filter(|(_, f)| !f()).map(|(n, _)| *n).collect();
        if failing.is_empty() || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    // Checked after the others settled: 08 must still be queued.
    let fresh_still_queued = r("inbox/planner/08.json").is_file();

    uninstall(handle);
    for pid in panes_by_agent.values() {
        let _ = registry.kill_pane(pid, &pool).await;
    }

    for &a in AGENT_IDS {
        println!("===== {a}.log =====\n{}", log(a).replace('\x1b', "<ESC>"));
    }
    assert!(fresh_still_queued, "08 fresh message to not-ready planner must stay in inbox");
    assert!(failing.is_empty(), "failing checks: {failing:#?}");
}

//! Opt-in end-to-end test of the bridge against REAL PTY panes.
//!
//! Each of the 9 agents is a real pane running a tiny fake agent (a
//! python script that appends every character it receives on stdin to
//! a per-agent log file). Envelopes are dropped into the sender outbox exactly
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

/// Atomic outbox drop (`.tmp` + rename), optionally back-dating the
/// file so time-based retry expiry can be exercised without waiting.
fn drop_raw(root: &Path, sender: &str, name: &str, content: &str, age: Option<Duration>) {
    let dir = root.join("outbox").join(sender);
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

/// Spawn one real PTY pane per agent running [`FAKE_AGENT`], each
/// logging what it receives to `<work>/<agent>.log`. With
/// `tester_awaits_approval`, integration-tester runs as `claude-code`
/// and prints a line the approval regex matches, so its pane is stuck in
/// `awaiting_approval` (the H4 scenario).
async fn spawn_fake_agents(
    app: &tauri::App<tauri::test::MockRuntime>,
    registry: &TerminalRegistry,
    pool: &crate::db::DbPool,
    work: &Path,
    tester_awaits_approval: bool,
) -> (HashMap<String, String>, HashMap<&'static str, PathBuf>) {
    let script = work.join("fake_agent.py");
    fs::write(&script, FAKE_AGENT).expect("script");
    let python =
        std::env::var("NEURON_E2E_PYTHON").unwrap_or_else(|_| "python".into());
    let mut panes_by_agent: HashMap<String, String> = HashMap::new();
    let mut logs: HashMap<&'static str, PathBuf> = HashMap::new();
    for &agent in AGENT_IDS {
        let log = work.join(format!("{agent}.log"));
        fs::write(&log, "").expect("log");
        let (kind, banner) = if tester_awaits_approval && agent == "integration-tester" {
            ("claude-code", " 'Approve this change?'")
        } else {
            ("shell", "")
        };
        let cmd = format!("{python} '{}' '{}'{banner}", fwd(&script), fwd(&log));
        let pane = registry
            .spawn_pane(
                PaneSpawnInput {
                    cwd: fwd(work),
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
    (panes_by_agent, logs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns 9 real PTY panes running python — opt in via --ignored"]
async fn bridge_end_to_end_with_fake_agents() {
    let (app, pool, _db_dir) = mock_app_with_pool_and_terminal_registry().await;
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let work = tempfile::tempdir().expect("tempdir");

    // ---- 9 fake-agent panes ----------------------------------------- //
    let (panes_by_agent, logs) =
        spawn_fake_agents(&app, &registry, &pool, work.path(), true).await;
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
    drop_raw(&root, "coordinator", "aa-newer",
        r#"{"from":"coordinator","to":"scout","body":"order-second"}"#, None);
    drop_raw(&root, "coordinator", "zz-older",
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
    drop_raw(&root, "frontend-reviewer", "01",
        r#"{"from":"frontend-reviewer","to":"planner","body":"plan-review-01"}"#, None);
    // H2: reply edges added in wave 2 — reviewer and tester can answer
    // the orchestrator, scout can answer a builder.
    drop_raw(&root, "backend-reviewer", "09",
        r#"{"from":"backend-reviewer","to":"orchestrator","body":"reply-09"}"#, None);
    drop_raw(&root, "scout", "10",
        r#"{"from":"scout","to":"frontend-builder","body":"reply-10"}"#, None);
    // Security: an oversized envelope is rejected unread.
    let huge = format!(
        r#"{{"from":"coordinator","to":"scout","body":"HUGE-{}"}}"#,
        "x".repeat(300 * 1024)
    );
    drop_raw(&root, "coordinator", "11", &huge, None);
    // Security: a symlink in an outbox is rejected without following it
    // (skipped when the OS refuses to create symlinks, e.g. Windows
    // without Developer Mode).
    let secret = work.path().join("secret.txt");
    fs::write(&secret, "TOP-SECRET-12").expect("secret");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&secret, root.join("outbox/coordinator/12.json")).is_ok();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&secret, root.join("outbox/coordinator/12.json")).is_ok();
    println!("symlink scenario active: {linked}");
    // The same non-regular-file gate, exercisable on every platform: a
    // directory named like an envelope.
    fs::create_dir_all(root.join("outbox/coordinator/13.json/inner")).expect("dir entry");
    // Security: scout claims to be the orchestrator → refused, scout told.
    drop_raw(&root, "scout", "14",
        r#"{"from":"orchestrator","to":"planner","body":"spoof-14"}"#, None);
    // Legacy protocol: a drop into inbox/<target>/ is never delivered;
    // the claimed sender is pointed at its outbox.
    fs::create_dir_all(root.join("inbox/coordinator")).expect("legacy inbox");
    fs::write(
        root.join("inbox/coordinator/15.json"),
        r#"{"from":"scout","to":"coordinator","body":"legacy-15"}"#,
    )
    .expect("legacy drop");
    // H5: capitalised sender, numeric task_id, raw newline in body.
    drop_raw(&root, "coordinator", "02",
        "{\"from\":\"Coordinator\",\"to\":\"backend-builder\",\"body\":\"satir-bir\nsatir-iki\",\"task_id\":42}", None);
    // Structurally invalid (no body) → rejected after retries, sender told.
    drop_raw(&root, "scout", "03",
        r#"{"from":"scout","to":"coordinator"}"#, None);
    // H3 + H7: decorated DONE with a note → review fanout carrying it.
    drop_raw(&root, "backend-builder", "04",
        r#"{"from":"backend-builder","to":"coordinator","body":"**DONE** T4 — auth.rs: refresh eklendi","task_id":"T4"}"#, None);
    // H3: "tamam —" completion with a task_id → sender told it was ignored.
    drop_raw(&root, "frontend-builder", "05",
        r#"{"from":"frontend-builder","to":"coordinator","body":"tamam — login sayfasi bitti","task_id":"T5"}"#, None);
    // H4: target pane stuck in awaiting_approval must still receive.
    drop_raw(&root, "coordinator", "06",
        r#"{"from":"coordinator","to":"integration-tester","body":"smoke-run-06"}"#, None);
    // H6: not-ready target, message already older than the grace → rejected + sender told.
    drop_raw(&root, "coordinator", "07",
        r#"{"from":"coordinator","to":"planner","body":"plan-07"}"#,
        Some(Duration::from_secs(120)));
    // H6: not-ready target, fresh message → must stay queued (old code dropped it after ~1 s).
    drop_raw(&root, "coordinator", "08",
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
        ("02 signature carries the numeric task_id", Box::new(|| log("backend-builder").contains("from @coordinator [task 42] [routed by Neuron]"))),
        // Unparseable → target unknown → filed under the (path) sender.
        ("03 invalid envelope → rejected/scout", Box::new(|| r("rejected/scout/03.json").is_file())),
        ("03 sender scout got [Neuron] notice", Box::new(|| { let l = log("scout"); l.contains("[Neuron]") && l.contains("missing field `body`") })),
        ("04 DONE delivered to coordinator", Box::new(|| log("coordinator").contains("**DONE** T4"))),
        // The fake agent's console input drops `—` (python msvcrt), so the
        // separator is not asserted — only that the note follows the id.
        ("04 reviewer got review WITH note", Box::new(|| { let l = log("backend-reviewer"); l.contains("review T4") && l.contains("auth.rs: refresh eklendi") })),
        ("05 frontend-builder told signal not recognised", Box::new(|| log("frontend-builder").contains("TANINMADI"))),
        ("06 awaiting_approval pane still received", Box::new(|| log("integration-tester").contains("smoke-run-06"))),
        ("07 expired not-ready → rejected/planner", Box::new(|| r("rejected/planner/07.json").is_file())),
        ("07 sender coordinator got [Neuron] notice", Box::new(|| log("coordinator").contains("never became ready"))),
        ("11 oversized envelope rejected unread", Box::new(|| {
            read(&r("rejected/coordinator/11.reason")).contains("too large") && !log("scout").contains("HUGE-")
        })),
        ("12 outbox symlink rejected without being followed", Box::new(move || {
            !linked || (read(&r("rejected/coordinator/12.reason")).contains("not a regular file")
                && !log("planner").contains("TOP-SECRET"))
        })),
        ("13 non-regular outbox entry rejected unread", Box::new(|| {
            read(&r("rejected/coordinator/13.reason")).contains("not a regular file")
        })),
        ("14 spoofed `from` refused, never delivered", Box::new(|| {
            read(&r("rejected/planner/14.reason")).contains("outbox/scout") && !log("planner").contains("spoof-14")
        })),
        ("14 spoofing sender scout got [Neuron] notice", Box::new(|| log("scout").contains("outbox/scout"))),
        ("15 legacy inbox drop rejected, never delivered", Box::new(|| {
            read(&r("rejected/coordinator/15.reason")).contains("legacy inbox") && !log("coordinator").contains("legacy-15")
        })),
        ("15 claimed sender scout pointed at its outbox", Box::new(|| log("scout").contains("legacy inbox"))),
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
    let fresh_still_queued = r("outbox/coordinator/08.json").is_file();

    uninstall(handle);
    for pid in panes_by_agent.values() {
        let _ = registry.kill_pane(pid, &pool).await;
    }

    for &a in AGENT_IDS {
        println!("===== {a}.log =====\n{}", log(a).replace('\x1b', "<ESC>"));
    }
    assert!(fresh_still_queued, "08 fresh message to not-ready planner must stay queued");
    assert!(failing.is_empty(), "failing checks: {failing:#?}");
}

/// Sets an env var for the rest of a test and restores it on drop, so a
/// failing step cannot leak a test-only override into the process.
struct EnvOverride(&'static str);

impl EnvOverride {
    fn set(key: &'static str, value: &str) -> Self {
        std::env::set_var(key, value);
        Self(key)
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        std::env::remove_var(self.0);
    }
}

/// Poll `cond` every 100 ms for up to `secs`.
async fn wait_until(secs: u64, cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The task-lifecycle FAILURE paths (swarm-logic review A1–A6) driven
/// through real panes and the real watcher: the test plays every agent
/// by dropping envelopes into their outboxes one step at a time, and
/// asserts on what the bridge pasted into each pane. Bodies are ASCII —
/// the fake agent's console input drops characters like `—`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns 9 real PTY panes running python — opt in via --ignored"]
async fn lifecycle_failure_paths_with_fake_agents() {
    let (app, pool, _db_dir) = mock_app_with_pool_and_terminal_registry().await;
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let work = tempfile::tempdir().expect("tempdir");
    let (panes_by_agent, logs) =
        spawn_fake_agents(&app, &registry, &pool, work.path(), false).await;
    let log = |a: &str| read(&logs[a]);
    let count = |a: &str, needle: &str| log(a).matches(needle).count();
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let root = work.path().join("bridge");
    prepare_layout(&root).expect("layout");
    let ready: HashSet<String> = panes_by_agent.values().cloned().collect();
    let handle = install(
        app.handle().clone(),
        root.clone(),
        panes_by_agent.clone(),
        Arc::new(Mutex::new(ready)),
        Arc::new(LifecycleStore::new()),
    );

    let seq = std::cell::Cell::new(0u32);
    let send = |from: &str, to: &str, body: &str, task_id: Option<&str>| {
        seq.set(seq.get() + 1);
        let mut env = serde_json::json!({ "from": from, "to": to, "body": body });
        if let Some(id) = task_id {
            env["task_id"] = id.into();
        }
        drop_raw(&root, from, &format!("m{:03}", seq.get()), &env.to_string(), None);
    };
    let mut failed: Vec<String> = Vec::new();
    macro_rules! step {
        ($name:expr, $cond:expr) => {
            if !wait_until(10, || $cond).await {
                failed.push($name.to_string());
            }
        };
    }

    // ---- A: reviewer rejects → coordinator re-dispatches → re-review -- //
    send("orchestrator", "backend-builder", "build R1", Some("R1"));
    step!("A1 builder got the dispatch", log("backend-builder").contains("build R1"));
    send("backend-builder", "coordinator", "DONE R1 - v1 auth.rs", Some("R1"));
    step!("A2 reviewer got review R1", count("backend-reviewer", "review R1") == 1);
    send("backend-reviewer", "coordinator", "CHANGES_NEEDED R1 - add a test", Some("R1"));
    step!("A3 coordinator got the rejection", log("coordinator").contains("CHANGES_NEEDED R1"));
    send("coordinator", "backend-builder", "R1 fix-round-1 add a test", Some("R1"));
    step!("A4 builder got the fix dispatch", log("backend-builder").contains("fix-round-1"));
    send("backend-builder", "coordinator", "DONE R1 - v2 test added", Some("R1"));
    step!("A5 reviewer re-reviews the fix", count("backend-reviewer", "review R1") == 2
        && log("backend-reviewer").contains("v2 test added"));
    send("backend-reviewer", "coordinator", "APPROVED R1", Some("R1"));
    step!("A6 orchestrator got TASK_DONE R1", log("orchestrator").contains("TASK_DONE R1"));

    // ---- B: tester FAIL → fix → review → RETEST by the tester -------- //
    send("orchestrator", "integration-tester", "smoke S1 run cargo test", Some("S1"));
    step!("B1 tester got the smoke run", log("integration-tester").contains("smoke S1"));
    send("integration-tester", "coordinator",
        "CHANGES_NEEDED S1 - test_parse fails in parse.rs, backend-builder", Some("S1"));
    step!("B2 coordinator got the test failure", log("coordinator").contains("CHANGES_NEEDED S1"));
    send("coordinator", "backend-builder", "S1 fix test_parse", Some("S1"));
    step!("B3 builder got the fix dispatch", log("backend-builder").contains("S1 fix test_parse"));
    send("backend-builder", "coordinator", "DONE S1 - parse.rs fixed", Some("S1"));
    step!("B4 the fix is reviewed first", log("backend-reviewer").contains("review S1"));
    step!("B5 no retest before the review", !log("integration-tester").contains("retest S1"));
    send("backend-reviewer", "coordinator", "APPROVED S1 - looks right", Some("S1"));
    step!("B6 tester got retest S1 with the builder's note",
        log("integration-tester").contains("retest S1") && log("integration-tester").contains("parse.rs fixed"));
    step!("B7 no TASK_DONE before the retest passes", !log("orchestrator").contains("TASK_DONE S1"));
    send("integration-tester", "coordinator", "APPROVED S1", Some("S1"));
    step!("B8 orchestrator got TASK_DONE S1 naming the fix",
        log("orchestrator").contains("TASK_DONE S1") && log("orchestrator").contains("parse.rs fixed"));

    // ---- C: builder hata (misaddressed) → TASK_FAILED; id is then closed //
    send("orchestrator", "frontend-builder", "build H1", Some("H1"));
    step!("C1 builder got the dispatch", log("frontend-builder").contains("build H1"));
    send("frontend-builder", "orchestrator", "hata - npm install is not allowed", Some("H1"));
    step!("C2 hata re-addressed to the coordinator", log("coordinator").contains("npm install is not allowed"));
    step!("C3 sender told about the re-addressing", log("frontend-builder").contains("@coordinator'a iletildi"));
    step!("C4 orchestrator got TASK_FAILED H1 with the reason",
        log("orchestrator").contains("TASK_FAILED H1") && log("orchestrator").contains("npm install is not allowed"));
    step!("C5 coordinator told not to re-dispatch H1", log("coordinator").contains("TASK_FAILED H1"));
    send("coordinator", "frontend-builder", "retry H1", Some("H1"));
    step!("C6 dispatch of a closed id refused", !log("frontend-builder").contains("retry H1")
        && log("coordinator").contains("already closed"));

    // ---- D: rework limit — the 3rd rejection fails the task ----------- //
    send("orchestrator", "backend-builder", "build L1", Some("L1"));
    step!("D0 builder got L1", log("backend-builder").contains("build L1"));
    for round in 1..=3usize {
        send("backend-builder", "coordinator", &format!("DONE L1 - attempt {round}"), Some("L1"));
        step!(format!("D{round} review L1 #{round}"), count("backend-reviewer", "review L1") == round);
        send("backend-reviewer", "coordinator", &format!("CHANGES_NEEDED L1 - still wrong {round}"), Some("L1"));
        step!(format!("D{round} rejection #{round} delivered"), count("coordinator", "CHANGES_NEEDED L1") == round);
        if round < 3 {
            send("coordinator", "backend-builder", &format!("L1 rework {round}"), Some("L1"));
            step!(format!("D{round} rework dispatched"), log("backend-builder").contains(&format!("L1 rework {round}")));
        }
    }
    step!("D4 orchestrator got TASK_FAILED L1", log("orchestrator").contains("TASK_FAILED L1"));
    step!("D5 coordinator told the limit is used up", log("coordinator").contains("TASK_FAILED L1"));

    // ---- E: one open task id belongs to one builder (A6) -------------- //
    send("orchestrator", "backend-builder", "build D1 backend half", Some("D1"));
    step!("E1 backend-builder got D1", log("backend-builder").contains("D1 backend half"));
    send("orchestrator", "frontend-builder", "build D1 frontend half", Some("D1"));
    step!("E2 second builder refused for the same open id",
        !log("frontend-builder").contains("D1 frontend half")
            && log("orchestrator").contains("still open on @backend-builder"));

    // ---- F: misaddressed DONE still starts the review ----------------- //
    send("orchestrator", "frontend-builder", "build F1", Some("F1"));
    step!("F1 builder got F1", log("frontend-builder").contains("build F1"));
    send("frontend-builder", "orchestrator", "DONE F1 - Login.tsx", Some("F1"));
    step!("F2 DONE sent to the orchestrator still reached the reviewer",
        log("frontend-reviewer").contains("review F1"));

    // ---- H: a task_id only in the body is still tracked; its hata fails it //
    send("orchestrator", "backend-builder", "build H2 now. task_id=H2. Report DONE H2", None);
    step!("H1 signature carries the body's task id",
        log("backend-builder").contains("from @orchestrator [task H2] [routed by Neuron]"));
    send("backend-builder", "coordinator", "hata - no permission for this", Some("H2"));
    step!("H2 orchestrator got TASK_FAILED H2", log("orchestrator").contains("TASK_FAILED H2"));

    // ---- G: an unanswered dispatch is reminded, escalated, given up --- //
    // Short thresholds from here on (restored on drop, even on panic);
    // earlier open tasks may be reminded too — only ST1's texts count.
    let _stall_override = EnvOverride::set("NEURON_TERM_STALL_SECS", "2");
    send("orchestrator", "frontend-builder", "build ST1", Some("ST1"));
    step!("G1 builder got ST1", log("frontend-builder").contains("build ST1"));
    if !wait_until(15, || log("frontend-builder").contains("DONE ST1")).await {
        failed.push("G2 silent builder reminded about ST1".into());
    }
    if !wait_until(15, || {
        let l = log("coordinator");
        l.contains("`ST1`") && l.contains("hatırlatmaya cevap gelmedi")
    })
    .await
    {
        failed.push("G3 coordinator got the ST1 escalation".into());
    }
    if !wait_until(15, || {
        let l = log("orchestrator");
        l.contains("TASK_FAILED ST1") && l.contains("yanıt vermedi")
    })
    .await
    {
        failed.push("G4 nobody answered: ST1 given up as TASK_FAILED".into());
    }
    drop(_stall_override);

    uninstall(handle);
    for pid in panes_by_agent.values() {
        let _ = registry.kill_pane(pid, &pool).await;
    }
    for &a in AGENT_IDS {
        println!("===== {a}.log =====\n{}", log(a).replace('\x1b', "<ESC>"));
    }
    assert!(failed.is_empty(), "failing steps: {failed:#?}");
}

//! End-to-end test of the bridge against REAL PTY panes, run by CI and
//! opt-in locally (it opens 9 consoles).
//!
//! Each of the 9 agents is a real pane running a tiny fake agent (a
//! PowerShell script that appends every key its console receives to a
//! per-agent log file). Envelopes are dropped into the sender outbox exactly
//! as a claude `Write` call would, the real watcher delivers them, and
//! the test asserts on the filesystem queue + what each pane received.
//!
//! Run with:
//!   cargo test --lib swarm_term::bridge::e2e_tests -- --ignored --nocapture

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use tauri::Manager;

use crate::models::PaneSpawnInput;
use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::AGENT_IDS;
use crate::swarm_term::lifecycle::LifecycleStore;
use crate::test_support::mock_app_with_pool_and_terminal_registry;

use super::{install, prepare_layout, uninstall, BridgeControl};

/// Fake agent: optional banner line (`$Banner`) on stdout, then append
/// every received key to `$Log`. `[Console]::ReadKey($true)` reads the
/// console key by key (no line buffering, no echo). Windows PowerShell
/// ships with Windows, so the test needs no interpreter of its own.
const FAKE_AGENT: &str = r#"
param([string]$Log, [string]$Banner)
if ($Banner) { [Console]::Out.WriteLine($Banner) }
$out = New-Object System.IO.StreamWriter($Log, $true, (New-Object System.Text.UTF8Encoding($false)))
$out.AutoFlush = $true
while ($true) { $out.Write([Console]::ReadKey($true).KeyChar) }
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
    let script = work.join("fake_agent.ps1");
    fs::write(&script, FAKE_AGENT).expect("script");
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
        let cmd = format!(
            "powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File '{}' '{}'{banner}",
            fwd(&script),
            fwd(&log)
        );
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
#[ignore = "spawns 9 real PTY panes with fake agents — CI runs it; opt in via --ignored"]
async fn bridge_end_to_end_with_fake_agents() {
    let (app, pool, _db_dir) = mock_app_with_pool_and_terminal_registry().await;
    let registry = app.state::<TerminalRegistry>().inner().clone();
    let work = tempfile::tempdir().expect("tempdir");

    // ---- 9 fake-agent panes ----------------------------------------- //
    let (panes_by_agent, logs) =
        spawn_fake_agents(&app, &registry, &pool, work.path(), true).await;
    let log = |a: &str| read(&logs[a]);

    // Let the fake agents boot, and wait for the approval false
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
        Arc::new(RwLock::new(panes_by_agent.clone())),
        Arc::new(Mutex::new(ready)),
        Arc::new(LifecycleStore::new()),
        Arc::new(BridgeControl::default()),
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
    // Item 9: the builder declared its files before that DONE (older
    // mtime → processed first, whatever the write order).
    drop_raw(&root, "backend-builder", "03b",
        r#"{"from":"backend-builder","to":"coordinator","body":"BUILDING T4 — dosyalar: src/auth.rs","task_id":"T4"}"#,
        Some(Duration::from_secs(5)));
    // H3 + H7: decorated DONE with a note → review fanout carrying it.
    drop_raw(&root, "backend-builder", "04",
        r#"{"from":"backend-builder","to":"coordinator","body":"**DONE** T4 — auth.rs: refresh eklendi; cargo test ok","task_id":"T4"}"#, None);
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
        ("04 reviewer got review WITH note", Box::new(|| log("backend-reviewer").contains("review T4 — auth.rs: refresh eklendi"))),
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
/// asserts on what the bridge pasted into each pane.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns 9 real PTY panes with fake agents — CI runs it; opt in via --ignored"]
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
        Arc::new(RwLock::new(panes_by_agent.clone())),
        Arc::new(Mutex::new(ready)),
        Arc::new(LifecycleStore::new()),
        Arc::new(BridgeControl::default()),
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
    send("backend-builder", "coordinator", "BUILDING R1 - dosyalar: src/auth.rs", Some("R1"));
    step!("A1b the builder declared its files", count("coordinator", "BUILDING R1") == 1);
    send("backend-builder", "coordinator", "DONE R1 - v1 auth.rs; cargo test ok", Some("R1"));
    step!("A2 reviewer got review R1", count("backend-reviewer", "review R1") == 1);
    send("backend-reviewer", "coordinator", "CHANGES_NEEDED R1 - no test covers parse(): add one", Some("R1"));
    step!("A3 coordinator got the rejection", log("coordinator").contains("CHANGES_NEEDED R1"));
    send("coordinator", "backend-builder", "R1 fix-round-1 add a test", Some("R1"));
    step!("A4 builder got the fix dispatch", log("backend-builder").contains("fix-round-1"));
    send("backend-builder", "coordinator", "DONE R1 - v2 test added; cargo test passed", Some("R1"));
    step!("A5 reviewer re-reviews the fix", count("backend-reviewer", "review R1") == 2
        && log("backend-reviewer").contains("v2 test added"));
    send("backend-reviewer", "coordinator", "APPROVED R1", Some("R1"));
    // A review alone closes nothing: the tester runs it first.
    // The declaration alone: the DONE notes' prose (`auth.rs`) is not a list.
    step!("A6 tester got test R1 with the builder's note and the declared files",
        log("integration-tester").contains("test R1") && log("integration-tester").contains("v2 test added")
            && log("integration-tester").contains("| dosyalar: src/auth.rs"));
    step!("A7 no TASK_DONE before the tester passes", !log("orchestrator").contains("TASK_DONE R1"));
    send("integration-tester", "coordinator", "APPROVED R1 - cargo test: 40 passed", Some("R1"));
    step!("A8 orchestrator got TASK_DONE R1", log("orchestrator").contains("TASK_DONE R1"));
    // The bridge root is `<parent>/bridge`, so the experience file lands
    // in the parent, as `<project>/.bridgespace/experience.md` would.
    step!("A9 the passed task left an experience card",
        std::fs::read_to_string(work.path().join("experience.md"))
            .is_ok_and(|t| t.contains("✓") && t.contains("R1:") && t.contains("doğrulama: cargo test")));

    // ---- B: tester FAIL → fix → review → RETEST by the tester -------- //
    send("orchestrator", "integration-tester", "smoke S1 run cargo test", Some("S1"));
    step!("B1 tester got the smoke run", log("integration-tester").contains("smoke S1"));
    send("integration-tester", "coordinator",
        "CHANGES_NEEDED S1 - test_parse fails in parse.rs, backend-builder", Some("S1"));
    step!("B2 coordinator got the test failure", log("coordinator").contains("CHANGES_NEEDED S1"));
    send("coordinator", "backend-builder", "S1 fix test_parse", Some("S1"));
    step!("B3 builder got the fix dispatch", log("backend-builder").contains("S1 fix test_parse"));
    send("backend-builder", "coordinator", "BUILDING S1 - dosyalar: src/parse.rs", Some("S1"));
    step!("B3b the builder declared its files", count("coordinator", "BUILDING S1") == 1);
    send("backend-builder", "coordinator", "DONE S1 - parse.rs fixed; cargo test passed", Some("S1"));
    step!("B4 the fix is reviewed first", log("backend-reviewer").contains("review S1"));
    step!("B5 no retest before the review", !log("integration-tester").contains("retest S1"));
    send("backend-reviewer", "coordinator", "APPROVED S1 - looks right", Some("S1"));
    step!("B6 tester got retest S1 with the builder's note and the declared files",
        log("integration-tester").contains("retest S1") && log("integration-tester").contains("parse.rs fixed")
            && log("integration-tester").contains("retest S1 — ")
            && log("integration-tester").contains("| dosyalar: src/parse.rs"));
    step!("B7 no TASK_DONE before the retest passes", !log("orchestrator").contains("TASK_DONE S1"));
    send("integration-tester", "coordinator", "APPROVED S1 - testler koşulamadı: node_modules yok", Some("S1"));
    step!("B7a a tester APPROVED that ran nothing is refused",
        count("integration-tester", "KABUL EDİLMEDİ") == 1 && !log("orchestrator").contains("TASK_DONE S1"));
    send("integration-tester", "coordinator", "APPROVED S1", Some("S1"));
    step!("B7b a tester APPROVED without its run is refused",
        count("integration-tester", "KABUL EDİLMEDİ") == 2 && !log("orchestrator").contains("TASK_DONE S1"));
    send("integration-tester", "coordinator", "APPROVED S1 - cargo test: 12 passed", Some("S1"));
    step!("B8 orchestrator got TASK_DONE S1 naming the fix",
        log("orchestrator").contains("TASK_DONE S1") && log("orchestrator").contains("parse.rs fixed"));

    // ---- C: builder hata (misaddressed) → TASK_FAILED; id is then closed //
    send("orchestrator", "frontend-builder", "build H1", Some("H1"));
    step!("C1 builder got the dispatch", log("frontend-builder").contains("build H1"));
    send("frontend-builder", "orchestrator", "hata - npm install is not allowed", Some("H1"));
    step!("C2 hata re-addressed to the coordinator", log("coordinator").contains("npm install is not allowed"));
    step!("C3 sender told about the re-addressing", log("frontend-builder").contains("@coordinator'a iletildi"));
    step!("C4 orchestrator got TASK_FAILED H1 with the reason and the MAST label",
        log("orchestrator").contains("TASK_FAILED H1") && log("orchestrator").contains("npm install is not allowed")
            && log("orchestrator").contains("TASK_FAILED H1 — [FM-3.1]"));
    step!("C5 coordinator told not to re-dispatch H1", log("coordinator").contains("TASK_FAILED H1"));
    send("coordinator", "frontend-builder", "retry H1", Some("H1"));
    step!("C6 dispatch of a closed id refused", !log("frontend-builder").contains("retry H1")
        && log("coordinator").contains("already closed"));

    // ---- D: rework limit — the 3rd rejection fails the task ----------- //
    send("orchestrator", "backend-builder", "build L1", Some("L1"));
    step!("D0 builder got L1", log("backend-builder").contains("build L1"));
    send("backend-builder", "coordinator", "BUILDING L1 - dosyalar: src/l1.rs", Some("L1"));
    step!("D0b the builder declared its files", count("coordinator", "BUILDING L1") == 1);
    for round in 1..=3usize {
        send("backend-builder", "coordinator", &format!("DONE L1 - attempt {round}; npm test passed"), Some("L1"));
        step!(format!("D{round} review L1 #{round}"), count("backend-reviewer", "review L1") == round);
        send("backend-reviewer", "coordinator", &format!("CHANGES_NEEDED L1 - test_x fails, attempt {round} still wrong"), Some("L1"));
        step!(format!("D{round} rejection #{round} delivered"), count("coordinator", "CHANGES_NEEDED L1") == round);
        if round < 3 {
            send("coordinator", "backend-builder", &format!("L1 rework {round}"), Some("L1"));
            step!(format!("D{round} rework dispatched"), log("backend-builder").contains(&format!("L1 rework {round}")));
        }
    }
    step!("D4 orchestrator got TASK_FAILED L1 labelled step repetition",
        log("orchestrator").contains("TASK_FAILED L1 — [FM-1.3]"));
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
    send("frontend-builder", "coordinator", "BUILDING F1 - dosyalar: Login.tsx", Some("F1"));
    step!("F1b the builder declared its files", count("coordinator", "BUILDING F1") == 1);
    send("frontend-builder", "orchestrator", "DONE F1 - Login.tsx; pnpm test passed", Some("F1"));
    step!("F2 DONE sent to the orchestrator still reached the reviewer",
        log("frontend-reviewer").contains("review F1"));

    // ---- I: a DONE without run evidence is refused, not reviewed ----- //
    send("orchestrator", "backend-builder", "build V1", Some("V1"));
    step!("I0 builder got V1", log("backend-builder").contains("build V1"));
    send("backend-builder", "coordinator", "BUILDING V1 - dosyalar: src/auth.rs", Some("V1"));
    step!("I0b the builder declared its files", count("coordinator", "BUILDING V1") == 1);
    send("backend-builder", "coordinator", "DONE V1 - auth.rs changed", Some("V1"));
    step!("I1 the builder is told its DONE was refused",
        log("backend-builder").contains("KABUL EDİLMEDİ") && !log("backend-reviewer").contains("review V1"));
    send("backend-builder", "coordinator", "DONE V1 - auth.rs changed; doğrulama: cargo test → 3 geçti, exit 0", Some("V1"));
    step!("I2 the review starts once the evidence is there", log("backend-reviewer").contains("review V1"));

    // ---- J: a hata with a cause holds the task for the user ----------- //
    send("orchestrator", "frontend-builder", "build P1", Some("P1"));
    step!("J0 builder got P1", log("frontend-builder").contains("build P1"));
    send("frontend-builder", "coordinator", "hata - [izin] npm install denied", Some("P1"));
    step!("J1 coordinator told the task is held for the user", log("coordinator").contains("BEKLEMEDE"));
    step!("J2 no TASK_FAILED for a held task", !log("orchestrator").contains("TASK_FAILED P1"));
    send("coordinator", "frontend-builder", "P1 again, install is fixed", Some("P1"));
    step!("J3 a held id can be dispatched again", log("frontend-builder").contains("P1 again"));

    // ---- K: a code rejection without evidence is refused -------------- //
    send("orchestrator", "backend-builder", "build K1", Some("K1"));
    step!("K0 builder got K1", log("backend-builder").contains("build K1"));
    send("backend-builder", "coordinator", "BUILDING K1 - dosyalar: src/k.rs", Some("K1"));
    step!("K0b the builder declared its files", count("coordinator", "BUILDING K1") == 1);
    send("backend-builder", "coordinator", "DONE K1 - k.rs; cargo test passed", Some("K1"));
    step!("K1 reviewer got review K1", log("backend-reviewer").contains("review K1"));
    send("backend-reviewer", "coordinator", "CHANGES_NEEDED K1 - rewrite it, I dislike it", Some("K1"));
    step!("K2 the reviewer is told its rejection was refused", log("backend-reviewer").contains("KABUL EDİLMEDİ"));

    // ---- H: a task_id only in the body is still tracked; its hata fails it //
    send("orchestrator", "backend-builder", "build H2 now. task_id=H2. Report DONE H2", None);
    step!("H1 signature carries the body's task id",
        log("backend-builder").contains("from @orchestrator [task H2] [routed by Neuron]"));
    send("backend-builder", "coordinator", "hata - no permission for this", Some("H2"));
    step!("H2 orchestrator got TASK_FAILED H2", log("orchestrator").contains("TASK_FAILED H2"));

    // ---- L: two open tasks declare one file; a DONE needs a declaration //
    send("orchestrator", "backend-builder", "build C1 shared part one", Some("C1"));
    step!("L0 backend-builder got C1", log("backend-builder").contains("build C1"));
    send("backend-builder", "coordinator", "BUILDING C1 - dosyalar: src/shared.rs, src/c1.rs", Some("C1"));
    step!("L1 the first declaration raises no conflict",
        count("coordinator", "BUILDING C1") == 1 && !log("coordinator").contains("aynı dosya"));
    send("orchestrator", "frontend-builder", "build C2 shared part two", Some("C2"));
    step!("L2 frontend-builder got C2", log("frontend-builder").contains("build C2"));
    send("frontend-builder", "coordinator", "BUILDING C2 - dosyalar: src/shared.rs", Some("C2"));
    step!("L3 the second builder is told the file is claimed by C1",
        log("frontend-builder").contains("kapanmadan bu dosyaya yazma") && log("frontend-builder").contains("`C1`"));
    step!("L4 the coordinator is told to sequence the two tasks",
        log("coordinator").contains("aynı dosya") && log("coordinator").contains("src/shared.rs"));
    send("orchestrator", "backend-builder", "build C3 no declaration", Some("C3"));
    step!("L5 backend-builder got C3", log("backend-builder").contains("build C3"));
    send("backend-builder", "coordinator", "DONE C3 - src/c3.rs; doğrulama: cargo test → 2 geçti, exit 0", Some("C3"));
    step!("L6 a DONE without a file declaration is refused",
        log("backend-builder").contains("dosya beyanı yok") && !log("backend-reviewer").contains("review C3"));
    send("backend-builder", "coordinator", "BUILDING C3 - dosyalar: yok", Some("C3"));
    step!("L7 the empty declaration is accepted", count("coordinator", "BUILDING C3") == 1);
    send("backend-builder", "coordinator", "DONE C3 - src/c3.rs; doğrulama: cargo test → 2 geçti, exit 0", Some("C3"));
    step!("L8 the review starts once the declaration is in", log("backend-reviewer").contains("review C3"));
    send("backend-reviewer", "coordinator", "APPROVED C3", Some("C3"));
    step!("L9 C3 goes to the tester", log("integration-tester").contains("test C3"));
    send("integration-tester", "coordinator", "APPROVED C3 - cargo test: 2 passed", Some("C3"));
    step!("L10 orchestrator got TASK_DONE C3", log("orchestrator").contains("TASK_DONE C3"));

    // ---- M: a dispatch naming an open dependency waits for it --------- //
    send("orchestrator", "backend-builder", "build M1 base", Some("M1"));
    step!("M0 backend-builder got M1", log("backend-builder").contains("build M1"));
    send("orchestrator", "frontend-builder", "build M2 on top — bağımlılık: M1", Some("M2"));
    step!("M1 the orchestrator is told M2 is held",
        log("orchestrator").contains("bekletiliyor") && log("orchestrator").contains("`M1`"));
    step!("M2 the held dispatch is not delivered", !log("frontend-builder").contains("build M2"));
    send("backend-builder", "coordinator", "hata - M1 cannot be built", Some("M1"));
    step!("M3 orchestrator got TASK_FAILED M1", log("orchestrator").contains("TASK_FAILED M1"));
    step!("M4 the dependent dispatch comes back to its sender",
        log("orchestrator").contains("başarısız kapandı") && !log("frontend-builder").contains("build M2"));
    send("orchestrator", "backend-builder", "build M3 base two", Some("M3"));
    step!("M5 backend-builder got M3", log("backend-builder").contains("build M3"));
    send("orchestrator", "frontend-builder", "build M4 on top — bağımlılık: M3", Some("M4"));
    step!("M6 M4 is held too", count("orchestrator", "bekletiliyor") == 2);
    send("backend-builder", "coordinator", "BUILDING M3 - dosyalar: src/m3.rs", Some("M3"));
    step!("M7 M3 declared", count("coordinator", "BUILDING M3") == 1);
    send("backend-builder", "coordinator", "DONE M3 - src/m3.rs; doğrulama: cargo test → 1 geçti, exit 0", Some("M3"));
    step!("M8 M3 is reviewed", log("backend-reviewer").contains("review M3"));
    send("backend-reviewer", "coordinator", "APPROVED M3", Some("M3"));
    step!("M9 M3 is tested", log("integration-tester").contains("test M3"));
    send("integration-tester", "coordinator", "APPROVED M3 - cargo test: 1 passed", Some("M3"));
    step!("M10 orchestrator got TASK_DONE M3", log("orchestrator").contains("TASK_DONE M3"));
    step!("M11 the dependent dispatch is delivered once M3 closed", log("frontend-builder").contains("build M4"));

    // ---- N: the coordinator's own TASK_FAILED carries a MAST label ---- //
    send("orchestrator", "backend-builder", "build N1", Some("N1"));
    step!("N0 backend-builder got N1", log("backend-builder").contains("build N1"));
    send("coordinator", "orchestrator", "TASK_FAILED N1 — builder cannot get the API key", Some("N1"));
    step!("N1 the untagged give-up still closes the task", log("orchestrator").contains("TASK_FAILED N1"));
    send("orchestrator", "backend-builder", "retry N1", Some("N1"));
    step!("N1b the closed N1 cannot be re-dispatched",
        !log("backend-builder").contains("retry N1") && log("orchestrator").contains("`N1` already closed"));
    step!("N2 the coordinator is asked for the label", log("coordinator").contains("MAST etiketi yok"));
    send("orchestrator", "frontend-builder", "build N2", Some("N2"));
    step!("N3 frontend-builder got N2", log("frontend-builder").contains("build N2"));
    send("coordinator", "orchestrator", "TASK_FAILED N2 — [FM-2.3] builder wandered into a redesign", Some("N2"));
    step!("N4 the tagged give-up closes the task", log("orchestrator").contains("TASK_FAILED N2"));
    // A third, untagged one: its notice arrives after any (wrong) notice for
    // N2 would have, so the count pins "no notice for a tagged report".
    send("orchestrator", "backend-builder", "build N3", Some("N3"));
    step!("N5 backend-builder got N3", log("backend-builder").contains("build N3"));
    send("coordinator", "orchestrator", "TASK_FAILED N3 — no label again", Some("N3"));
    step!("N6 only the untagged give-ups were asked for a label",
        log("coordinator").contains("`TASK_FAILED N3` kabul edildi ama MAST etiketi yok")
            && count("coordinator", "MAST etiketi yok") == 2);

    // ---- Z: close what the scenarios left open ------------------------ //
    // G runs the stall clocks at 2 s. Every open task then stalls too, and
    // each costs the checker a pane paste per level; with nine of them
    // the checker lagged so far on CI that ST1 went from the reminder
    // straight to the give-up (level 3) and the escalation (level 2) was
    // never sent. A builder's untagged `hata` closes its own open task.
    for (builder, id) in [
        ("backend-builder", "D1"),
        ("frontend-builder", "F1"),
        ("backend-builder", "V1"),
        ("frontend-builder", "P1"),
        ("backend-builder", "K1"),
        ("backend-builder", "C1"),
        ("frontend-builder", "C2"),
        ("frontend-builder", "M4"),
    ] {
        send(builder, "coordinator", &format!("hata - {id} dropped before the stall scenario"), Some(id));
        step!(format!("Z {id} closed before the stall scenario"),
            log("orchestrator").contains(&format!("TASK_FAILED {id}")));
    }

    // ---- G: an unanswered dispatch is reminded, escalated, given up --- //
    // Short thresholds from here on (restored on drop, even on panic);
    // only ST1 is open now, so only ST1's clocks run.
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
        l.contains("TASK_FAILED ST1 — [FM-3.1]") && l.contains("yanıt vermedi")
    })
    .await
    {
        failed.push("G4 nobody answered: ST1 given up as TASK_FAILED".into());
    }
    // A question the planner never answers: the planner is reminded, then
    // the asker is told to go on — nothing is given up.
    send("coordinator", "planner", "plan the parser split", None);
    if !wait_until(15, || {
        let l = log("planner");
        l.contains("@coordinator") && l.contains("cevabını bekliyor")
    })
    .await
    {
        failed.push("G5 silent planner reminded of the question".into());
    }
    if !wait_until(15, || {
        let l = log("coordinator");
        l.contains("@planner") && l.contains("cevap vermedi")
    })
    .await
    {
        failed.push("G6 coordinator told the planner did not answer".into());
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

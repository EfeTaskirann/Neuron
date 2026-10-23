//! H3 measurement: what a reference run cost and how runs compare.
//!
//! - [`transcript_usage`]: the API usage in a claude transcript.
//! - [`RunRecord`]: one task's run — a line of the history file.
//! - [`trend_table`]: the history per task and team — pass rate, medians,
//!   and the newest run against the ones before it.
//! - [`pass_k`]: one suite invocation run k times — which tasks passed
//!   every run; [`failure_distribution`]: its TASK_FAILEDs by MAST label.
//!
//! Pure apart from the history file, so CI tests it.

use std::collections::{BTreeMap, HashMap};
use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// API calls and tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Usage {
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    pub(crate) fn add(&mut self, other: &Usage) {
        self.calls += other.calls;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }

    /// Every input token the model read: fresh, from the cache, and the
    /// ones written to it.
    pub(crate) fn input_total(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// The API usage in a claude transcript (JSON lines). claude writes a line
/// per content block of an assistant message, each carrying the whole
/// message's `usage`, so a message counts once — by `message.id`.
pub(crate) fn transcript_usage(jsonl: &str) -> Usage {
    let mut by_message: HashMap<String, Usage> = HashMap::new();
    let mut total = Usage::default();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if v["type"] != "assistant" {
            continue;
        }
        let usage = &v["message"]["usage"];
        if !usage.is_object() {
            continue;
        }
        let tokens = |key: &str| usage[key].as_u64().unwrap_or(0);
        let one = Usage {
            calls: 1,
            input: tokens("input_tokens"),
            output: tokens("output_tokens"),
            cache_read: tokens("cache_read_input_tokens"),
            cache_write: tokens("cache_creation_input_tokens"),
        };
        match v["message"]["id"].as_str() {
            Some(id) => {
                by_message.insert(id.to_string(), one);
            }
            None => total.add(&one),
        }
    }
    for one in by_message.values() {
        total.add(one);
    }
    total
}

/// One check of a run: what was checked, whether it held, and what was
/// seen when it did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Check {
    pub name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

impl Check {
    pub(crate) fn new(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Self {
        Self { name: name.into(), ok, detail: detail.into() }
    }
}

/// A failed task of a run: its id, the MAST code its `TASK_FAILED`
/// carried (`FM-3.1`; none when the coordinator wrote no label) and the
/// reason text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FailureNote {
    pub task: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub reason: String,
}

/// One reference task's run: a line of the history file. Times are
/// seconds; `_at` is unix seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RunRecord {
    /// Bumped when a field changes meaning.
    pub schema: u32,
    /// Which repetition of the suite (1-based; 0 in lines written before
    /// `NEURON_REF_REPEAT` existed).
    #[serde(default)]
    pub rep: u32,
    /// The suite run it was part of.
    pub suite: String,
    pub task: String,
    pub tier: String,
    /// `full`, `lite` or `custom`.
    pub team: String,
    pub started_at: i64,
    /// The Neuron commit under test (`-dirty` with local changes).
    pub neuron: String,
    pub claude_version: Option<String>,
    pub pass: bool,
    pub checks: Vec<Check>,
    /// Since the start: every pane ready, and the task sent.
    pub ready_s: f64,
    pub task_sent_s: f64,
    /// Since the task was sent: the first and the last terminal signal.
    pub first_signal_s: Option<f64>,
    pub last_signal_s: Option<f64>,
    /// Since the task was sent: the agents' last transcript write.
    #[serde(default)]
    pub last_activity_s: Option<f64>,
    /// The start to the end of the watch.
    pub total_s: f64,
    pub timed_out: bool,
    pub done_signals: u32,
    pub failed_signals: u32,
    /// The signals in order, `TASK_DONE <id>` / `TASK_FAILED <id>`.
    pub signals: Vec<String>,
    /// The failed tasks with their MAST labels and reasons.
    #[serde(default)]
    pub failures: Vec<FailureNote>,
    pub delivered: u32,
    pub rejected: u32,
    pub notices: u32,
    pub reminders: u32,
    pub budget_holds: u32,
    pub denied: u32,
    pub changed: Vec<String>,
    pub tokens: Usage,
    pub tokens_by_agent: BTreeMap<String, Usage>,
}

impl RunRecord {
    /// How long the swarm worked on the task: until its last terminal
    /// signal; without one (a refusal), until its last activity; else
    /// until the watch ended (records from before `last_activity_s`).
    pub(crate) fn work_s(&self) -> f64 {
        self.last_signal_s
            .or(self.last_activity_s)
            .unwrap_or(self.total_s - self.task_sent_s)
    }
}

/// Append `record` to the history file, creating it (and its folder).
pub(crate) fn append_history(file: &Path, record: &RunRecord) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    writeln!(f, "{line}")
}

/// The history, oldest first, and how many lines could not be read.
pub(crate) fn read_history(file: &Path) -> (Vec<RunRecord>, usize) {
    let Ok(text) = std::fs::read_to_string(file) else { return (Vec::new(), 0) };
    let mut skipped = 0;
    let records = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let r = serde_json::from_str::<RunRecord>(l).ok();
            skipped += usize::from(r.is_none());
            r
        })
        .collect();
    (records, skipped)
}

/// Half again as much is worth a flag.
const JUMP: f64 = 0.5;

/// One task's runs in a suite invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskPassK {
    pub task: String,
    pub team: String,
    pub runs: usize,
    pub passed: usize,
}

/// pass^k of one suite invocation: a task passes only if every one of its
/// runs passed (Bjarnason et al. 2026 — one run is a coin toss).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PassK {
    /// The most runs any task has.
    pub k: usize,
    pub tasks: usize,
    pub all_passed: usize,
    pub per_task: Vec<TaskPassK>,
}

impl PassK {
    pub(crate) fn render(&self) -> String {
        if self.tasks == 0 {
            return "pass^k: kayıt yok\n".into();
        }
        let fallen: Vec<String> = self
            .per_task
            .iter()
            .filter(|t| t.passed < t.runs)
            .map(|t| format!("{} {}/{}", t.task, t.passed, t.runs))
            .collect();
        let mut out = format!("pass^{}: {}/{} görev her koşuyu geçti", self.k, self.all_passed, self.tasks);
        if !fallen.is_empty() {
            out.push_str(&format!("; düşenler: {}", fallen.join(", ")));
        }
        out.push('\n');
        out
    }
}

/// Pure: pass^k over the records of ONE suite invocation (filter the
/// history by `suite` first), per task and team.
pub(crate) fn pass_k(records: &[RunRecord]) -> PassK {
    let mut rows: BTreeMap<(&str, &str), (usize, usize)> = BTreeMap::new();
    for r in records {
        let e = rows.entry((r.task.as_str(), r.team.as_str())).or_default();
        e.0 += 1;
        e.1 += usize::from(r.pass);
    }
    let per_task: Vec<TaskPassK> = rows
        .into_iter()
        .map(|((task, team), (runs, passed))| TaskPassK { task: task.into(), team: team.into(), runs, passed })
        .collect();
    PassK {
        k: per_task.iter().map(|t| t.runs).max().unwrap_or(0),
        tasks: per_task.len(),
        all_passed: per_task.iter().filter(|t| t.runs > 0 && t.passed == t.runs).count(),
        per_task,
    }
}

/// Pure: how often each MAST label closed a task in `records`, most
/// frequent first (`etiketsiz` for a label-less TASK_FAILED).
pub(crate) fn failure_distribution(records: &[RunRecord]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in records.iter().flat_map(|r| &r.failures) {
        *counts.entry(f.label.clone().unwrap_or_else(|| "etiketsiz".into())).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// One line of [`failure_distribution`].
pub(crate) fn render_distribution(distribution: &[(String, usize)]) -> String {
    if distribution.is_empty() {
        return "arıza dağılımı: TASK_FAILED yok\n".into();
    }
    let parts: Vec<String> = distribution.iter().map(|(label, n)| format!("{label} ×{n}")).collect();
    format!("arıza dağılımı: {}\n", parts.join(", "))
}

/// The trend table over `history` (oldest first), one row per task and
/// team: the last `window` runs as ✔/✘, the pass rate over them, the
/// medians of the work time and the tokens, and the newest run against
/// the median of the `base` runs before it. A pass turning into a fail
/// and a time or output-token jump are flagged. A "run" is one suite
/// invocation: its k repetitions (`NEURON_REF_REPEAT`) are one mark — ✔
/// only when every repetition passed — and one median.
pub(crate) fn trend_table(history: &[RunRecord], window: usize, base: usize) -> String {
    if history.is_empty() {
        return "Henüz kayıt yok.\n".into();
    }
    let mut rows: BTreeMap<(&str, &str), Vec<&RunRecord>> = BTreeMap::new();
    for r in history {
        rows.entry((r.task.as_str(), r.team.as_str())).or_default().push(r);
    }
    let mut out = String::from(
        "| görev | takım | son koşular | başarı | iş süresi | output token | input token | son koşu |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    let (mut passes, mut runs) = (0, 0);
    for ((task, team), all) in &rows {
        let invocations = by_suite(all);
        let recent = &invocations[invocations.len().saturating_sub(window)..];
        let marks: String = recent.iter().map(|inv| if all_passed(inv) { '✔' } else { '✘' }).collect();
        let pass = recent.iter().filter(|inv| all_passed(inv)).count();
        passes += pass;
        runs += recent.len();
        let records = || recent.iter().flatten();
        out.push_str(&format!(
            "| {task} | {team} | {marks} | {pass}/{} ({}%) | {} | {} | {} | {} |\n",
            recent.len(),
            percent(pass, recent.len()),
            secs(median(records().map(|r| r.work_s()))),
            tokens(median(records().map(|r| r.tokens.output as f64))),
            tokens(median(records().map(|r| r.tokens.input_total() as f64))),
            newest_vs_base(&invocations, base),
        ));
    }
    out.push_str(&format!(
        "\nToplam (görev başına son {window} koşu): {passes}/{runs} geçti ({}%).\n",
        percent(passes, runs)
    ));
    out
}

/// Pure: one task's records (oldest first) grouped by suite invocation —
/// consecutive records of one suite are its repetitions.
fn by_suite<'a>(all: &[&'a RunRecord]) -> Vec<Vec<&'a RunRecord>> {
    let mut groups: Vec<Vec<&'a RunRecord>> = Vec::new();
    for r in all {
        match groups.last_mut() {
            Some(g) if g[0].suite == r.suite => g.push(r),
            _ => groups.push(vec![r]),
        }
    }
    groups
}

fn all_passed(invocation: &[&RunRecord]) -> bool {
    invocation.iter().all(|r| r.pass)
}

/// Pure: the newest invocation against the medians of the `base`
/// invocations before it (an invocation's own value is the median of its
/// repetitions).
fn newest_vs_base(invocations: &[Vec<&RunRecord>], base: usize) -> String {
    let Some((newest, before)) = invocations.split_last() else { return String::new() };
    let before = &before[before.len().saturating_sub(base)..];
    if before.is_empty() {
        return "ilk koşu".into();
    }
    let mut notes = Vec::new();
    if !all_passed(newest) && before.iter().any(|inv| all_passed(inv)) {
        notes.push("✘ REGRESYON".to_string());
    }
    let mut compare = |what: &str, now: Option<f64>, then: Option<f64>| {
        let (Some(now), Some(then)) = (now, then.filter(|t| *t > 0.0)) else { return };
        let change = now / then - 1.0;
        let flag = if change > JUMP { " ⚠" } else { "" };
        notes.push(format!("{what} {:+.0}%{flag}", change * 100.0));
    };
    let before_records = || before.iter().flatten();
    compare(
        "süre",
        median(newest.iter().map(|r| r.work_s())),
        median(before_records().map(|r| r.work_s())),
    );
    compare(
        "output",
        median(newest.iter().map(|r| r.tokens.output as f64)),
        median(before_records().map(|r| r.tokens.output as f64)),
    );
    notes.join(", ")
}

fn median(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut v: Vec<f64> = values.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len() % 2 == 0 { (v[mid - 1] + v[mid]) / 2.0 } else { v[mid] })
}

fn percent(part: usize, whole: usize) -> usize {
    (part * 100 + whole / 2).checked_div(whole).unwrap_or(0)
}

/// `4m10s`, `42s`; `—` for none.
fn secs(s: Option<f64>) -> String {
    match s {
        None => "—".into(),
        Some(s) => {
            let s = s.round() as u64;
            if s >= 60 {
                format!("{}m{:02}s", s / 60, s % 60)
            } else {
                format!("{s}s")
            }
        }
    }
}

/// `15.2k`, `1.9M`; `—` for none.
fn tokens(t: Option<f64>) -> String {
    match t {
        None => "—".into(),
        Some(t) if t >= 1e6 => format!("{:.1}M", t / 1e6),
        Some(t) if t >= 1e3 => format!("{:.1}k", t / 1e3),
        Some(t) => format!("{t:.0}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(id: Option<&str>, output: u64, cache_read: u64) -> String {
        let id = id.map(|id| format!(r#""id":"{id}","#)).unwrap_or_default();
        format!(
            r#"{{"type":"assistant","message":{{{id}"role":"assistant","content":[],"usage":{{"input_tokens":2,"output_tokens":{output},"cache_read_input_tokens":{cache_read},"cache_creation_input_tokens":10}}}}}}"#
        )
    }

    #[test]
    fn a_message_counts_once_however_many_lines_carry_it() {
        let transcript = [
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
            assistant(Some("msg_a"), 100, 1000),
            // The same message's next content block: same usage, same id.
            assistant(Some("msg_a"), 100, 1000),
            assistant(Some("msg_b"), 40, 2000),
            "not json".to_string(),
            r#"{"type":"assistant","message":{"id":"msg_c","content":[]}}"#.to_string(),
            // No id to go by: every such line counts.
            assistant(None, 5, 0),
        ]
        .join("\n");
        assert_eq!(
            transcript_usage(&transcript),
            Usage { calls: 3, input: 6, output: 145, cache_read: 3000, cache_write: 30 }
        );
        assert_eq!(transcript_usage(&transcript).input_total(), 3036);
        assert_eq!(transcript_usage(""), Usage::default());
    }

    fn run(task: &str, pass: bool, work: f64, output: u64) -> RunRecord {
        RunRecord {
            schema: 1,
            rep: 1,
            failures: Vec::new(),
            suite: "ref-1".into(),
            task: task.into(),
            tier: "smoke".into(),
            team: "full".into(),
            started_at: 1_790_000_000,
            neuron: "abc1234".into(),
            claude_version: Some("2.1.296".into()),
            pass,
            checks: vec![Check::new("TASK_DONE", pass, "")],
            ready_s: 12.0,
            task_sent_s: 13.0,
            first_signal_s: Some(work / 2.0),
            last_signal_s: Some(work),
            last_activity_s: None,
            total_s: 13.0 + work + 90.0,
            timed_out: false,
            done_signals: 1,
            failed_signals: 0,
            signals: vec!["TASK_DONE T1".into()],
            delivered: 20,
            rejected: 0,
            notices: 1,
            reminders: 0,
            budget_holds: 0,
            denied: 0,
            changed: vec!["src/orders.rs".into()],
            tokens: Usage { calls: 40, input: 80, output, cache_read: 1_500_000, cache_write: 200_000 },
            tokens_by_agent: BTreeMap::new(),
        }
    }

    /// Each record its own suite invocation (one run each).
    fn invocations(mut history: Vec<RunRecord>) -> Vec<RunRecord> {
        for (i, r) in history.iter_mut().enumerate() {
            r.suite = format!("ref-{}", i + 1);
        }
        history
    }

    #[test]
    fn the_trend_table_flags_a_new_failure_and_a_jump() {
        let history = invocations(vec![
            run("bug-off-by-one", true, 100.0, 15_000),
            run("bug-off-by-one", true, 120.0, 17_000),
            run("feature-discount", true, 200.0, 20_000),
            // Newest: failed, twice as slow, output about the same.
            run("bug-off-by-one", false, 220.0, 16_500),
        ]);
        let table = trend_table(&history, 10, 5);
        let row = |task: &str| {
            table.lines().find(|l| l.starts_with(&format!("| {task} |"))).unwrap_or_default().to_string()
        };
        let bug = row("bug-off-by-one");
        assert!(bug.contains("| ✔✔✘ |") && bug.contains("2/3 (67%)"), "{bug}");
        // Median work time of the three runs: 120 s.
        assert!(bug.contains("| 2m00s |"), "{bug}");
        assert!(bug.contains("✘ REGRESYON"), "{bug}");
        // 220 against the median 110 of the two before: +100%, flagged.
        assert!(bug.contains("süre +100% ⚠"), "{bug}");
        // 16.5k against 16k: +3%, not flagged.
        assert!(bug.contains("output +3%") && !bug.contains("output +3% ⚠"), "{bug}");
        assert!(row("feature-discount").contains("ilk koşu"), "{table}");
        assert!(table.contains("3/4 geçti (75%)"), "{table}");
        assert_eq!(trend_table(&[], 10, 5), "Henüz kayıt yok.\n");
    }

    #[test]
    fn only_the_last_window_counts_and_teams_are_kept_apart() {
        let mut history: Vec<RunRecord> =
            invocations((0..12).map(|i| run("t", i >= 2, 60.0, 1000)).collect());
        let mut lite = run("t", false, 30.0, 500);
        lite.team = "lite".into();
        lite.suite = "ref-lite".into();
        history.push(lite);
        let table = trend_table(&history, 10, 5);
        // The two old failures fell out of the window of ten.
        assert!(table.contains("| t | full | ✔✔✔✔✔✔✔✔✔✔ | 10/10 (100%) |"), "{table}");
        assert!(table.contains("| t | lite | ✘ | 0/1 (0%) |"), "{table}");
    }

    #[test]
    fn the_history_is_appended_and_read_back_past_bad_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("nested").join("history.jsonl");
        assert_eq!(read_history(&file), (Vec::new(), 0));
        append_history(&file, &run("a", true, 10.0, 1)).expect("append");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .and_then(|mut f| writeln!(f, "{{broken"))
            .expect("garbage line");
        append_history(&file, &run("b", false, 20.0, 2)).expect("append");
        let (records, skipped) = read_history(&file);
        assert_eq!(records.iter().map(|r| r.task.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(records[1], run("b", false, 20.0, 2));
        assert_eq!(skipped, 1);
    }

    #[test]
    fn k_repetitions_of_one_invocation_are_one_run_in_the_trend() {
        // ref-1 ran `a` once (pass, 60 s); ref-2 ran it three times, one of
        // them failing, at twice the time: one ✘ mark, a regression and a
        // time jump against ref-1 — not rep 3 against reps 1–2.
        let mut history = vec![run("a", true, 60.0, 1000)];
        for (rep, pass) in [(1, true), (2, false), (3, true)] {
            let mut r = run("a", pass, 120.0, 1000);
            r.suite = "ref-2".into();
            r.rep = rep;
            history.push(r);
        }
        let table = trend_table(&history, 10, 5);
        let row = table.lines().find(|l| l.starts_with("| a |")).unwrap_or_default().to_string();
        assert!(row.contains("| ✔✘ |") && row.contains("1/2 (50%)"), "{row}");
        assert!(row.contains("✘ REGRESYON") && row.contains("süre +100% ⚠"), "{row}");
        assert!(table.contains("1/2 geçti (50%)"), "{table}");
    }

    #[test]
    fn pass_k_counts_the_tasks_that_passed_every_run() {
        let mut history = Vec::new();
        for rep in 1..=3u32 {
            for (task, pass) in [("a", true), ("b", rep != 2), ("c", true)] {
                let mut r = run(task, pass, 60.0, 1000);
                r.rep = rep;
                history.push(r);
            }
        }
        // A record of another suite is not this invocation's.
        let mut other = run("a", false, 60.0, 1000);
        other.suite = "ref-0".into();
        history.push(other);
        let mine: Vec<RunRecord> = history.iter().filter(|r| r.suite == "ref-1").cloned().collect();
        let pk = pass_k(&mine);
        assert_eq!((pk.k, pk.tasks, pk.all_passed), (3, 3, 2));
        let b = pk.per_task.iter().find(|t| t.task == "b").expect("b");
        assert_eq!((b.runs, b.passed), (3, 2));
        let text = pk.render();
        assert!(text.contains("pass^3") && text.contains("2/3 görev") && text.contains("b 2/3"), "{text}");
        // One run each: pass^1 is the plain pass count.
        let single = pass_k(&mine[..3]);
        assert_eq!((single.k, single.all_passed), (1, 3));
        assert_eq!(pass_k(&[]).render(), "pass^k: kayıt yok\n");
    }

    #[test]
    fn the_failure_distribution_counts_labels_and_the_unlabelled() {
        let mut a = run("a", false, 60.0, 1000);
        a.failures = vec![
            FailureNote { task: "T1".into(), label: Some("FM-3.1".into()), reason: "hata".into() },
            FailureNote { task: "T2".into(), label: Some("FM-1.3".into()), reason: "limit".into() },
        ];
        let mut b = run("b", false, 60.0, 1000);
        b.failures = vec![
            FailureNote { task: "T3".into(), label: Some("FM-3.1".into()), reason: "stall".into() },
            FailureNote { task: "T4".into(), label: None, reason: "gave up".into() },
        ];
        assert_eq!(
            failure_distribution(&[a.clone(), b.clone()]),
            vec![("FM-3.1".to_string(), 2), ("FM-1.3".to_string(), 1), ("etiketsiz".to_string(), 1)]
        );
        assert!(failure_distribution(&[run("c", true, 1.0, 1)]).is_empty());
        // Old history lines carry neither field.
        let mut line = serde_json::to_value(&a).expect("json");
        let obj = line.as_object_mut().expect("object");
        obj.remove("rep");
        obj.remove("failures");
        let read: RunRecord = serde_json::from_value(line).expect("a record without the fields still reads");
        assert_eq!((read.rep, read.failures.len()), (0, 0));
    }

    #[test]
    fn work_time_falls_back_to_the_last_activity_then_the_watch() {
        let mut r = run("t", true, 100.0, 1);
        r.last_activity_s = Some(130.0);
        // A signal ends the work, whatever followed it.
        assert_eq!(r.work_s(), 100.0);
        // A refusal sends none: the swarm's last activity ends it.
        r.last_signal_s = None;
        assert_eq!(r.work_s(), 130.0);
        // An early record has neither: the watch's end.
        r.last_activity_s = None;
        r.total_s = 300.0;
        assert_eq!(r.work_s(), 287.0);
        let mut line = serde_json::to_value(&r).expect("json");
        line.as_object_mut().expect("object").remove("last_activity_s");
        let read: RunRecord = serde_json::from_value(line).expect("a record without the field still reads");
        assert_eq!(read, r);
        assert_eq!(secs(Some(59.6)), "1m00s");
        assert_eq!(secs(Some(42.0)), "42s");
        assert_eq!(tokens(Some(1_940_000.0)), "1.9M");
        assert_eq!(tokens(Some(15_240.0)), "15.2k");
        assert_eq!(tokens(None), "—");
    }
}

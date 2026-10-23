//! H3 measurement: what a reference run cost and how runs compare.
//!
//! - [`transcript_usage`]: the API usage in a claude transcript.
//! - [`RunRecord`]: one task's run — a line of the history file.
//! - [`trend_table`]: the history per task and team — pass rate, medians,
//!   and the newest run against the ones before it.
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

/// One reference task's run: a line of the history file. Times are
/// seconds; `_at` is unix seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct RunRecord {
    /// Bumped when a field changes meaning.
    pub schema: u32,
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

/// The trend table over `history` (oldest first), one row per task and
/// team: the last `window` runs as ✔/✘, the pass rate over them, the
/// medians of the work time and the tokens, and the newest run against
/// the median of the `base` runs before it. A pass turning into a fail
/// and a time or output-token jump are flagged.
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
        let recent = &all[all.len().saturating_sub(window)..];
        let marks: String = recent.iter().map(|r| if r.pass { '✔' } else { '✘' }).collect();
        let pass = recent.iter().filter(|r| r.pass).count();
        passes += pass;
        runs += recent.len();
        out.push_str(&format!(
            "| {task} | {team} | {marks} | {pass}/{} ({}%) | {} | {} | {} | {} |\n",
            recent.len(),
            percent(pass, recent.len()),
            secs(median(recent.iter().map(|r| r.work_s()))),
            tokens(median(recent.iter().map(|r| r.tokens.output as f64))),
            tokens(median(recent.iter().map(|r| r.tokens.input_total() as f64))),
            newest_vs_base(all, base),
        ));
    }
    out.push_str(&format!(
        "\nToplam (görev başına son {window} koşu): {passes}/{runs} geçti ({}%).\n",
        percent(passes, runs)
    ));
    out
}

/// Pure: the newest of `runs` against the median of the `base` before it.
fn newest_vs_base(runs: &[&RunRecord], base: usize) -> String {
    let Some((newest, before)) = runs.split_last() else { return String::new() };
    let before = &before[before.len().saturating_sub(base)..];
    if before.is_empty() {
        return "ilk koşu".into();
    }
    let mut notes = Vec::new();
    if !newest.pass && before.iter().any(|r| r.pass) {
        notes.push("✘ REGRESYON".to_string());
    }
    let mut compare = |what: &str, now: f64, then: Option<f64>| {
        let Some(then) = then.filter(|t| *t > 0.0) else { return };
        let change = now / then - 1.0;
        let flag = if change > JUMP { " ⚠" } else { "" };
        notes.push(format!("{what} {:+.0}%{flag}", change * 100.0));
    };
    compare("süre", newest.work_s(), median(before.iter().map(|r| r.work_s())));
    compare(
        "output",
        newest.tokens.output as f64,
        median(before.iter().map(|r| r.tokens.output as f64)),
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

    #[test]
    fn the_trend_table_flags_a_new_failure_and_a_jump() {
        let history = vec![
            run("bug-off-by-one", true, 100.0, 15_000),
            run("bug-off-by-one", true, 120.0, 17_000),
            run("feature-discount", true, 200.0, 20_000),
            // Newest: failed, twice as slow, output about the same.
            run("bug-off-by-one", false, 220.0, 16_500),
        ];
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
        let mut history: Vec<RunRecord> = (0..12).map(|i| run("t", i >= 2, 60.0, 1000)).collect();
        let mut lite = run("t", false, 30.0, 500);
        lite.team = "lite".into();
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

//! Token accounting per task, read from the panes' transcripts.
//!
//! claude writes every answer's `usage` (input, cache creation, cache
//! read, output tokens) into the transcript the session monitor already
//! tails for the usage limit and compactions. [`usage_signal`] reads it,
//! [`TokenLedger`] charges it to the tasks open on that pane, and the
//! monitor tells the coordinator when a task passes its budget
//! ([`warn_notice`]) and closes it at twice the budget ([`stop_texts`]).
//! Without this the only cost control was the message count, and one
//! runaway task could spend the whole session.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::swarm_term::lifecycle::TASK_FAILED_PREFIX;

/// One answer's cost, as the transcript records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Usage {
    /// `message.id` — the same answer is written as several lines (text,
    /// tool use), each with the same usage; the ledger counts it once.
    pub message_id: String,
    /// Input + cache creation + cache read + output tokens.
    pub tokens: u64,
}

/// Pure: read one transcript line for an answer's usage.
pub(crate) fn usage_signal(line: &str) -> Option<Usage> {
    // Cheap pre-check: user turns and tool results carry no usage.
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let message = v.get("message")?;
    let message_id = message.get("id").and_then(Value::as_str)?.to_string();
    let usage = message.get("usage")?.as_object()?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let tokens = count("input_tokens")
        + count("cache_creation_input_tokens")
        + count("cache_read_input_tokens")
        + count("output_tokens");
    Some(Usage { message_id, tokens })
}

/// Which line a task's spending crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crossing {
    /// The budget: the coordinator is told.
    Warn,
    /// Twice the budget: the task is closed.
    Stop,
}

/// A task that crossed a line, reported once per line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Overrun {
    pub task_id: String,
    pub spent: u64,
    pub crossing: Crossing,
}

/// Tokens spent per task in one session.
#[derive(Debug, Default)]
pub(crate) struct TokenLedger {
    /// Per task; `0`: no budget, nothing ever crosses.
    budget: u64,
    total: u64,
    /// Agent → the last answer counted (dedupes the streamed lines).
    last_message: BTreeMap<String, String>,
    by_task: BTreeMap<String, u64>,
    warned: BTreeSet<String>,
    stopped: BTreeSet<String>,
}

impl TokenLedger {
    pub fn new(budget: u64) -> Self {
        Self { budget, ..Self::default() }
    }

    /// Charge `agent`'s answer to the `tasks` open on its pane (all of
    /// them — a pane rarely holds two) and to the session; report the
    /// lines it crossed.
    pub fn observe(&mut self, agent: &str, usage: &Usage, tasks: &[String]) -> Vec<Overrun> {
        if self.last_message.get(agent).is_some_and(|id| *id == usage.message_id) {
            return Vec::new();
        }
        self.last_message.insert(agent.to_string(), usage.message_id.clone());
        self.total = self.total.saturating_add(usage.tokens);
        let mut crossed = Vec::new();
        for id in tasks {
            let spent = {
                let entry = self.by_task.entry(id.clone()).or_insert(0);
                *entry = entry.saturating_add(usage.tokens);
                *entry
            };
            if self.budget == 0 {
                continue;
            }
            if spent >= self.budget.saturating_mul(2) && self.stopped.insert(id.clone()) {
                crossed.push(Overrun { task_id: id.clone(), spent, crossing: Crossing::Stop });
            } else if spent >= self.budget && self.warned.insert(id.clone()) {
                crossed.push(Overrun { task_id: id.clone(), spent, crossing: Crossing::Warn });
            }
        }
        crossed
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn budget(&self) -> u64 {
        self.budget
    }

    #[cfg(test)]
    pub fn spent_on(&self, task_id: &str) -> u64 {
        self.by_task.get(task_id).copied().unwrap_or(0)
    }
}

/// `3,2M`, `850k`, `12`.
pub(crate) fn fmt_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let tenths = (tokens + 50_000) / 100_000;
        if tenths % 10 == 0 {
            format!("{}M", tenths / 10)
        } else {
            format!("{},{}M", tenths / 10, tenths % 10)
        }
    } else if tokens >= 1_000 {
        format!("{}k", (tokens + 500) / 1_000)
    } else {
        tokens.to_string()
    }
}

/// The coordinator's notice when `task_id` passed its budget.
pub(crate) fn warn_notice(task_id: &str, spent: u64, budget: u64) -> String {
    format!(
        "`{task_id}` PAHALILAŞIYOR: bu görev şimdiye dek {} token harcadı (bütçe {}). {} \
         token'da Neuron görevi kapatır ve orchestrator'a `TASK_FAILED {task_id}` iletir. \
         Kapsamı daralt, görevi böl ya da builder'a nerede takıldığını sor.",
        fmt_tokens(spent),
        fmt_tokens(budget),
        fmt_tokens(budget.saturating_mul(2))
    )
}

/// At twice the budget: the `TASK_FAILED` body for the orchestrator,
/// the coordinator's notice, and the notice for the agents holding it.
pub(crate) fn stop_texts(task_id: &str, spent: u64, budget: u64) -> (String, String, String) {
    let failed = format!(
        "{TASK_FAILED_PREFIX}{task_id} — {} token bütçesinin iki katı aşıldı ({} harcandı, bütçe \
         {}); Neuron görevi kapattı. Kalan iş yeni bir task_id ile, daha dar kapsamla verilmeli.",
        crate::swarm_term::mast::TOKEN_OVERRUN.tag(),
        fmt_tokens(spent),
        fmt_tokens(budget)
    );
    let coordinator = format!(
        "`{task_id}` DURDURULDU: token bütçesinin iki katı aşıldı ({} harcandı, bütçe {}). \
         Orchestrator'a `TASK_FAILED {task_id}` iletildi. Bu task_id ile yeniden dispatch ETME; \
         kalan iş gerekiyorsa yeni bir task_id ile, daha dar kapsamla.",
        fmt_tokens(spent),
        fmt_tokens(budget)
    );
    let holder = format!(
        "`{task_id}` DURDURULDU: token bütçesinin iki katı aşıldı ({} harcandı). Bu görevi \
         bırak, DONE yazma; yeni dispatch bekle.",
        fmt_tokens(spent)
    );
    (failed, coordinator, holder)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STREAMED: &str = r#"{"parentUuid":"a","type":"assistant","message":{"id":"msg_01","model":"claude-opus-5-5","role":"assistant","content":[{"type":"text","text":"ok"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":30599,"cache_read_input_tokens":43521,"output_tokens":862,"service_tier":"standard"}},"requestId":"req_1","uuid":"b"}"#;

    #[test]
    fn usage_signal_sums_the_four_token_counts_of_an_assistant_message() {
        let u = usage_signal(STREAMED).unwrap();
        assert_eq!((u.message_id.as_str(), u.tokens), ("msg_01", 2 + 30_599 + 43_521 + 862));
        // A count claude leaves out is zero, not a refusal.
        let short = r#"{"type":"assistant","message":{"id":"msg_02","usage":{"input_tokens":5,"output_tokens":7}}}"#;
        assert_eq!(usage_signal(short).map(|u| u.tokens), Some(12));
        // No usage (claude's synthetic error entries), no id, or not an
        // answer: nothing.
        assert_eq!(
            usage_signal(r#"{"type":"assistant","isApiErrorMessage":true,"message":{"model":"<synthetic>","role":"assistant","content":[]}}"#),
            None
        );
        assert_eq!(usage_signal(r#"{"type":"assistant","message":{"usage":{"input_tokens":5}}}"#), None);
        assert_eq!(usage_signal(r#"{"type":"user","message":{"id":"msg_03","usage":{"input_tokens":5}}}"#), None);
        assert_eq!(usage_signal("not json"), None);
    }

    fn msg(id: &str, tokens: u64) -> Usage {
        Usage { message_id: id.into(), tokens }
    }

    fn t1() -> Vec<String> {
        vec!["T1".to_string()]
    }

    #[test]
    fn the_ledger_counts_a_streamed_message_once_and_charges_the_open_task() {
        let mut l = TokenLedger::new(1_000);
        // claude writes one answer as several transcript lines (text, tool
        // use), each carrying the same usage: count it once.
        assert!(l.observe("backend-builder", &msg("m1", 600), &t1()).is_empty());
        assert!(l.observe("backend-builder", &msg("m1", 600), &t1()).is_empty());
        assert_eq!((l.total(), l.spent_on("T1")), (600, 600));
        // A pane holding no task adds to the session only.
        assert!(l.observe("coordinator", &msg("c1", 100), &[]).is_empty());
        assert_eq!((l.total(), l.spent_on("T1")), (700, 600));
        // The budget is crossed once; twice the budget stops the task, once.
        assert_eq!(
            l.observe("backend-builder", &msg("m2", 500), &t1()),
            vec![Overrun { task_id: "T1".into(), spent: 1_100, crossing: Crossing::Warn }]
        );
        assert!(l.observe("backend-builder", &msg("m3", 500), &t1()).is_empty());
        assert_eq!(
            l.observe("backend-builder", &msg("m4", 500), &t1()),
            vec![Overrun { task_id: "T1".into(), spent: 2_100, crossing: Crossing::Stop }]
        );
        assert!(l.observe("backend-builder", &msg("m5", 500), &t1()).is_empty());
        assert_eq!(l.spent_on("T1"), 2_600);
        // Another agent's answer on the same task counts toward it too.
        assert!(l.observe("integration-tester", &msg("i1", 10), &t1()).is_empty());
        assert_eq!(l.spent_on("T1"), 2_610);
        assert_eq!(l.total(), 2_710);
        // No budget: nothing ever crosses.
        let mut free = TokenLedger::new(0);
        assert!(free.observe("backend-builder", &msg("m1", 5_000_000), &t1()).is_empty());
        assert_eq!(free.spent_on("T1"), 5_000_000);
    }

    #[test]
    fn notices_name_the_task_and_the_numbers() {
        assert_eq!(fmt_tokens(12), "12");
        assert_eq!(fmt_tokens(850_000), "850k");
        assert_eq!(fmt_tokens(3_000_000), "3M");
        assert_eq!(fmt_tokens(3_240_000), "3,2M");
        let w = warn_notice("T1", 3_240_000, 3_000_000);
        assert!(
            w.contains("`T1`") && w.contains("PAHALILAŞIYOR") && w.contains("3,2M") && w.contains("3M") && w.contains("6M"),
            "{w}"
        );
        let (failed, coordinator, holder) = stop_texts("T1", 6_100_000, 3_000_000);
        // Burning twice the budget is step repetition (MAST FM-1.3).
        assert!(failed.starts_with("TASK_FAILED T1 — [FM-1.3] ") && failed.contains("6,1M"), "{failed}");
        assert!(
            coordinator.contains("`T1`") && coordinator.contains("TASK_FAILED") && coordinator.contains("yeniden dispatch ETME"),
            "{coordinator}"
        );
        assert!(holder.contains("`T1`") && holder.contains("bırak"), "{holder}");
        for t in [w, failed, coordinator, holder] {
            assert!(!t.contains('\x1b') && !t.contains('\n'));
        }
    }
}

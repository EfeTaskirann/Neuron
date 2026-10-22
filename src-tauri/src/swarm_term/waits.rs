//! The wait graph: who waits for whom, and the cycles that are a
//! deadlock.
//!
//! The bridge sees every message; the session monitor sees every tool
//! call in the transcripts. From those two the graph knows, for each
//! agent, its newest message to each peer and its newest activity. An
//! agent WAITS on a peer when its last message to that peer is
//! unanswered and old, and the agent has been idle as long (it is not
//! working on something else). A cycle of such waits — A on B, B on C,
//! C on A — means nobody will ever move: the coordinator is told
//! ([`crate::swarm_term::bridge`]'s stall scan), once per cycle while
//! it stands. The lifecycle's task states cannot form such a cycle on
//! their own; the questions and reports agents send each other outside
//! the protocol can, and did in live runs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::Value;

/// `from` waits on `to`: its newest message there, unanswered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaitEdge {
    pub from: String,
    pub to: String,
    pub task_id: Option<String>,
    /// How long ago the message went.
    pub since: Duration,
}

/// A cycle of waits, in order: each edge's `to` is the next edge's
/// `from`, the last one's `to` is the first one's `from`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deadlock {
    pub edges: Vec<WaitEdge>,
}

/// Pure: an answer line of a transcript that calls a tool — the agent is
/// working (reading, editing, running), whatever it says.
pub fn activity_signal(line: &str) -> bool {
    // Cheap pre-check: most lines are not an answer with a tool call.
    if !line.contains("\"tool_use\"") || !line.contains("\"assistant\"") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    v.pointer("/message/content").and_then(Value::as_array).is_some_and(|blocks| {
        blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
    })
}

/// A message on record: when it went and the task it named.
type Sent = (Instant, Option<String>);

#[derive(Debug, Default)]
pub struct WaitGraph {
    /// `(from, to)` → the newest message that way, with its task id.
    last_sent: BTreeMap<(String, String), Sent>,
    /// Agent → its newest activity: a message sent, a tool called.
    last_activity: BTreeMap<String, Instant>,
    /// Cycles reported and still standing (their members, in cycle
    /// order from the smallest name); forgotten once they break, so a
    /// cycle that forms again is reported again.
    reported: BTreeSet<Vec<String>>,
}

impl WaitGraph {
    /// The bridge delivered a message `from` → `to`.
    pub fn note_message(&mut self, from: &str, to: &str, task_id: Option<&str>, now: Instant) {
        self.last_sent
            .insert((from.to_string(), to.to_string()), (now, task_id.map(str::to_string)));
        self.note_activity(from, now);
    }

    /// `agent` did something (a tool call in its transcript).
    pub fn note_activity(&mut self, agent: &str, now: Instant) {
        let last = self.last_activity.entry(agent.to_string()).or_insert(now);
        if now > *last {
            *last = now;
        }
    }

    /// The clocks stood still for `by` (a pause): nothing aged meanwhile.
    pub fn shift(&mut self, by: Duration) {
        for (t, _) in self.last_sent.values_mut() {
            *t += by;
        }
        for t in self.last_activity.values_mut() {
            *t += by;
        }
    }

    /// The cycles of waits older than `wait_after` among idle agents,
    /// each reported once while it stands.
    pub fn take_deadlocks(&mut self, now: Instant, wait_after: Duration) -> Vec<Deadlock> {
        let cycles = {
            // An edge: an old, unanswered message from an agent idle as long.
            let mut edges: BTreeMap<&str, Vec<(&str, &Sent)>> = BTreeMap::new();
            for ((from, to), sent) in &self.last_sent {
                let (t, _) = sent;
                if now.saturating_duration_since(*t) < wait_after {
                    continue;
                }
                let answered = self
                    .last_sent
                    .get(&(to.clone(), from.clone()))
                    .is_some_and(|(reply, _)| *reply > *t);
                let idle = self
                    .last_activity
                    .get(from)
                    .map_or(true, |a| now.saturating_duration_since(*a) >= wait_after);
                if answered || !idle {
                    continue;
                }
                edges.entry(from.as_str()).or_default().push((to.as_str(), sent));
            }
            let mut cycles: BTreeMap<Vec<String>, Vec<WaitEdge>> = BTreeMap::new();
            for start in edges.keys().copied() {
                find_cycles(start, &mut vec![start], &edges, now, &mut cycles);
            }
            cycles
        };
        let standing: BTreeSet<Vec<String>> = cycles.keys().cloned().collect();
        let fresh: Vec<Deadlock> = cycles
            .into_iter()
            .filter(|(members, _)| !self.reported.contains(members))
            .map(|(_, edges)| Deadlock { edges })
            .collect();
        self.reported = standing;
        fresh
    }
}

/// Every simple cycle reachable from `path`'s last node, keyed by its
/// members starting from the smallest name (so each cycle is found
/// once whichever node the search started from).
fn find_cycles<'a>(
    node: &'a str,
    path: &mut Vec<&'a str>,
    edges: &BTreeMap<&'a str, Vec<(&'a str, &'a Sent)>>,
    now: Instant,
    cycles: &mut BTreeMap<Vec<String>, Vec<WaitEdge>>,
) {
    let Some(nexts) = edges.get(node) else {
        return;
    };
    for (next, _) in nexts {
        if let Some(pos) = path.iter().position(|n| n == next) {
            let mut members: Vec<&str> = path[pos..].to_vec();
            let smallest = members.iter().enumerate().min_by_key(|(_, m)| **m).map(|(i, _)| i).unwrap_or(0);
            members.rotate_left(smallest);
            let key: Vec<String> = members.iter().map(|m| m.to_string()).collect();
            if cycles.contains_key(&key) {
                continue;
            }
            let cycle_edges = members
                .iter()
                .enumerate()
                .filter_map(|(i, from)| {
                    let to = members[(i + 1) % members.len()];
                    let (t, task_id) = edges.get(from)?.iter().find(|(peer, _)| *peer == to)?.1;
                    Some(WaitEdge {
                        from: from.to_string(),
                        to: to.to_string(),
                        task_id: task_id.clone(),
                        since: now.saturating_duration_since(*t),
                    })
                })
                .collect();
            cycles.insert(key, cycle_edges);
        } else {
            path.push(next);
            find_cycles(next, path, edges, now, cycles);
            path.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    fn edges(d: &Deadlock) -> Vec<(&str, &str, Option<&str>)> {
        d.edges.iter().map(|e| (e.from.as_str(), e.to.as_str(), e.task_id.as_deref())).collect()
    }

    /// builder → planner → coordinator → builder, one minute apart.
    fn triangle(t0: Instant) -> WaitGraph {
        let mut g = WaitGraph::default();
        g.note_message("backend-builder", "planner", Some("T1"), t0);
        g.note_message("planner", "coordinator", None, t0 + MIN);
        g.note_message("coordinator", "backend-builder", Some("T1"), t0 + 2 * MIN);
        g
    }

    #[test]
    fn a_cycle_of_idle_waiters_is_a_deadlock_reported_once() {
        let t0 = Instant::now();
        let mut g = triangle(t0);
        // Too fresh: nobody has waited long enough.
        assert!(g.take_deadlocks(t0 + 5 * MIN, 10 * MIN).is_empty());
        let found = g.take_deadlocks(t0 + 13 * MIN, 10 * MIN);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            edges(&found[0]),
            [
                ("backend-builder", "planner", Some("T1")),
                ("planner", "coordinator", None),
                ("coordinator", "backend-builder", Some("T1")),
            ]
        );
        assert_eq!(found[0].edges[0].since, 13 * MIN);
        // The same cycle is not reported again while it stands.
        assert!(g.take_deadlocks(t0 + 14 * MIN, 10 * MIN).is_empty());
        // One member works again: the cycle is broken — and reported anew
        // when it forms again.
        g.note_activity("planner", t0 + 15 * MIN);
        assert!(g.take_deadlocks(t0 + 16 * MIN, 10 * MIN).is_empty());
        g.note_message("planner", "coordinator", None, t0 + 16 * MIN);
        assert_eq!(g.take_deadlocks(t0 + 27 * MIN, 10 * MIN).len(), 1);
    }

    #[test]
    fn an_answered_message_or_a_busy_sender_is_no_wait() {
        let t0 = Instant::now();
        // Two agents: one of them always spoke last, so no cycle.
        let mut g = WaitGraph::default();
        g.note_message("coordinator", "backend-builder", Some("T1"), t0);
        g.note_message("backend-builder", "coordinator", Some("T1"), t0 + MIN);
        assert!(g.take_deadlocks(t0 + 15 * MIN, 10 * MIN).is_empty());
        // A sender still working (tool calls after its message) waits on
        // nobody; once it has been idle long enough, it does.
        let mut g = triangle(t0);
        g.note_activity("coordinator", t0 + 12 * MIN);
        assert!(g.take_deadlocks(t0 + 13 * MIN, 10 * MIN).is_empty());
        assert_eq!(g.take_deadlocks(t0 + 23 * MIN, 10 * MIN).len(), 1);
    }

    #[test]
    fn a_pause_does_not_age_the_waits() {
        let t0 = Instant::now();
        let mut g = triangle(t0);
        g.shift(10 * MIN);
        assert!(g.take_deadlocks(t0 + 13 * MIN, 10 * MIN).is_empty());
        assert_eq!(g.take_deadlocks(t0 + 23 * MIN, 10 * MIN).len(), 1);
    }

    #[test]
    fn activity_signal_is_a_tool_call_in_an_answer() {
        assert!(activity_signal(
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{}}]}}"#
        ));
        assert!(!activity_signal(
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"tool_use"}]}}"#
        ));
        assert!(!activity_signal(
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1"}]}}"#
        ));
        assert!(!activity_signal("not json"));
    }
}

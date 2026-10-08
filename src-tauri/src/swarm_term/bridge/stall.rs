//! Stalled-task reminders. A task that sits in `Building` or
//! `AwaitingReview` for too long gets a `[Neuron]` reminder pasted into
//! the responsible agent's pane; at twice the threshold the coordinator
//! is told too, so it can re-dispatch or escalate. Without this a task
//! whose review never came back waited silently forever.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::reviewer_for_builder;
use crate::swarm_term::lifecycle::{LifecycleState, LifecycleStore, Stall};

use super::notify::notify_agent;

/// How often the watcher scans the lifecycle store for stalls.
pub(super) const STALL_CHECK_EVERY: Duration = Duration::from_secs(30);

/// `AwaitingReview` longer than this → remind the paired reviewer.
const REVIEW_STALL_AFTER: Duration = Duration::from_secs(10 * 60);

/// `Building` longer than this → remind the builder.
const BUILD_STALL_AFTER: Duration = Duration::from_secs(20 * 60);

/// Scan for stalled tasks and send the due reminders.
pub(super) async fn check_stalls<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    panes_by_agent: &HashMap<String, String>,
    lifecycle: &LifecycleStore,
) {
    let stalls =
        lifecycle.take_stalls(Instant::now(), REVIEW_STALL_AFTER, BUILD_STALL_AFTER);
    for stall in stalls {
        let Some(source_agent) = panes_by_agent
            .iter()
            .find(|(_, pane)| **pane == stall.source_pane)
            .map(|(agent, _)| agent.as_str())
        else {
            continue;
        };
        let Some((responsible, text)) = reminder_for(source_agent, &stall) else {
            continue;
        };
        tracing::warn!(
            task_id = %stall.task_id,
            responsible = %responsible,
            state = ?stall.state,
            age_secs = stall.age.as_secs(),
            level = stall.level,
            "swarm-term bridge: stalled task — reminder sent"
        );
        notify_agent(registry, panes_by_agent, responsible, &text).await;
        emit_reminder(app, responsible, &text);
        if stall.level >= 2 {
            let text = escalation_for(responsible, &stall);
            notify_agent(registry, panes_by_agent, "coordinator", &text).await;
            emit_reminder(app, "coordinator", &text);
        }
    }
}

fn emit_reminder<R: Runtime>(app: &AppHandle<R>, target: &str, text: &str) {
    let _ = app.emit(
        crate::events::SWARM_TERM_ROUTE,
        json!({
            "source": "coordinator",
            "target": target,
            "body": format!("[Neuron] {text}"),
            "outcome": "lifecycle_fanout",
            "reason": "stalled task reminder",
        }),
    );
}

/// Pure: who is responsible for a stalled task and what to tell them.
/// `AwaitingReview` entries are keyed by the BUILDER's pane, so the
/// responsible agent is the builder's paired reviewer.
pub(super) fn reminder_for(
    source_agent: &str,
    stall: &Stall,
) -> Option<(&'static str, String)> {
    let id = &stall.task_id;
    let mins = stall.age.as_secs() / 60;
    match stall.state {
        LifecycleState::AwaitingReview => {
            let reviewer = reviewer_for_builder(source_agent)?;
            Some((
                reviewer,
                format!(
                    "hatırlatma: `review {id}` {mins} dakikadır cevapsız. İncelemeyi \
                     bitirip coordinator'a `APPROVED {id}` ya da `CHANGES_NEEDED {id} — \
                     <ne değişmeli>` yolla; takıldıysan coordinator'a `hata — <sebep>` yaz."
                ),
            ))
        }
        LifecycleState::Building => {
            let builder = crate::swarm_term::hierarchy::AGENT_IDS
                .iter()
                .copied()
                .find(|a| *a == source_agent)?;
            Some((
                builder,
                format!(
                    "hatırlatma: `{id}` görevi {mins} dakikadır BUILDING durumunda. \
                     Bittiyse coordinator'a `DONE {id} — <değişen dosyalar>` yolla; \
                     takıldıysan coordinator'a `hata — <sebep>` yaz."
                ),
            ))
        }
        _ => None,
    }
}

/// Pure: the level-2 notice telling the coordinator a task is stuck.
pub(super) fn escalation_for(responsible: &str, stall: &Stall) -> String {
    format!(
        "`{}` görevi {} dakikadır @{responsible}'da takılı ({:?}); hatırlatmaya \
         cevap gelmedi. Gerekirse yeniden dispatch et ya da orchestrator'a \
         `hata — <özet>` bildir.",
        stall.task_id,
        stall.age.as_secs() / 60,
        stall.state,
    )
}

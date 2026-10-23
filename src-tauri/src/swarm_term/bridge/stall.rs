//! Stalled-task reminders. A task that sits in `Assigned`, `Building`
//! or `AwaitingReview` for too long gets a `[Neuron]` reminder pasted
//! into the responsible agent's pane; at twice the threshold the
//! coordinator is told too, so it can re-dispatch or give the task up.
//! A rejection the coordinator never re-dispatched reminds the
//! coordinator, and escalates to the orchestrator. If nobody answers the
//! escalation either, the task is given up and reported `TASK_FAILED`.
//! Without this a task whose review never came back waited silently
//! forever.
//!
//! A question to an advisor (scout, planner) that gets no answer reminds
//! the advisor, and at twice the review threshold tells the asker to go
//! on without it; nothing is given up, an advisor owns no task.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::{reviewer_for_builder, AGENT_IDS};
use crate::swarm_term::lifecycle::{
    LifecycleState, LifecycleStore, Stall, Unanswered, MAX_REWORK_ROUNDS, TASK_FAILED_PREFIX,
    TESTER,
};

use super::notify::notify_agent;
use super::synth::enqueue_fanout;

/// How often the watcher scans the lifecycle store for stalls.
const STALL_CHECK_EVERY: Duration = Duration::from_secs(30);

/// `AwaitingReview` longer than this → remind the paired reviewer; a
/// rejection not re-dispatched for this long → remind the coordinator.
const REVIEW_STALL_AFTER: Duration = Duration::from_secs(10 * 60);

/// `Assigned` / `Building` longer than this → remind the builder (or
/// the tester, for its smoke / retest runs).
const BUILD_STALL_AFTER: Duration = Duration::from_secs(20 * 60);

/// Test override: `NEURON_TERM_STALL_SECS=<n>` sets BOTH thresholds to
/// `n` seconds (and scans at least every `n/2` s) so the reminder path
/// can be exercised without a 10-minute wait. Unset / unparsable / 0 →
/// the production defaults.
const STALL_OVERRIDE_ENV: &str = "NEURON_TERM_STALL_SECS";

fn stall_override() -> Option<u64> {
    std::env::var(STALL_OVERRIDE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
}

fn stall_thresholds() -> (Duration, Duration) {
    match stall_override() {
        Some(n) => (Duration::from_secs(n), Duration::from_secs(n)),
        None => (REVIEW_STALL_AFTER, BUILD_STALL_AFTER),
    }
}

/// Scan cadence: every 30 s, or faster under the test override.
pub(super) fn stall_check_every() -> Duration {
    match stall_override() {
        Some(n) => STALL_CHECK_EVERY.min(Duration::from_secs((n / 2).max(1))),
        None => STALL_CHECK_EVERY,
    }
}

/// How often the watcher re-reads the scan cadence (the test override is
/// an env var; reading it on every 250 ms tick was wasted work).
pub(super) const STALL_CADENCE_REFRESH: Duration = Duration::from_secs(5);

/// Scan for stalled tasks and send the due reminders; report the tasks
/// given up as `TASK_FAILED` to the orchestrator.
pub(super) async fn check_stalls<R: Runtime>(
    app: &AppHandle<R>,
    registry: &TerminalRegistry,
    root: &Path,
    panes_by_agent: &HashMap<String, String>,
    lifecycle: &LifecycleStore,
) {
    let (review_after, build_after) = stall_thresholds();
    let stalls = lifecycle.take_stalls(Instant::now(), review_after, build_after);
    for stall in stalls {
        let source_agent = panes_by_agent
            .iter()
            .find(|(_, pane)| **pane == stall.source_pane)
            .map(|(agent, _)| agent.as_str());
        let Some((responsible, text)) = reminder_for(source_agent, &stall) else {
            continue;
        };
        if stall.give_up {
            let (failed, notice) = give_up_texts(responsible, &stall);
            tracing::warn!(
                task_id = %stall.task_id,
                responsible = %responsible,
                age_secs = stall.age.as_secs(),
                "swarm-term bridge: stalled task given up — TASK_FAILED"
            );
            enqueue_fanout(app, root, panes_by_agent, "orchestrator", failed, &stall.task_id);
            notify_agent(registry, panes_by_agent, "coordinator", &notice).await;
            emit_reminder(app, "coordinator", &notice);
            continue;
        }
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
            let (to, text) = escalation_for(responsible, &stall);
            notify_agent(registry, panes_by_agent, to, &text).await;
            emit_reminder(app, to, &text);
        }
    }
    for question in lifecycle.take_unanswered(Instant::now(), review_after) {
        tracing::warn!(
            advisor = %question.advisor,
            asker = %question.asker,
            age_secs = question.age.as_secs(),
            level = question.level,
            "swarm-term bridge: unanswered question — reminder sent"
        );
        let reminder = question_reminder(&question);
        notify_agent(registry, panes_by_agent, &question.advisor, &reminder).await;
        emit_reminder(app, &question.advisor, &reminder);
        if question.level >= 2 {
            let notice = question_notice(&question);
            notify_agent(registry, panes_by_agent, &question.asker, &notice).await;
            emit_reminder(app, &question.asker, &notice);
        }
    }
}

/// Pure: the reminder for an advisor that has not answered a question.
pub(super) fn question_reminder(question: &Unanswered) -> String {
    format!(
        "hatırlatma: @{} {} cevabını bekliyor (sana bir mesaj yazdı). Cevabını ona \
         gönder; veremiyorsan nedenini kısaca yaz.",
        question.asker,
        since(question.age)
    )
}

/// Pure: what the asker is told once the advisor stayed silent past the
/// reminder too.
pub(super) fn question_notice(question: &Unanswered) -> String {
    format!(
        "@{} {} cevap vermedi (hatırlatmadan sonra da). Onsuz devam et ya da \
         sorunu yeniden, kısaca sor.",
        question.advisor,
        since(question.age)
    )
}

fn emit_reminder<R: Runtime>(app: &AppHandle<R>, target: &str, text: &str) {
    let _ = app.emit(
        crate::events::SWARM_TERM_ROUTE,
        json!({
            "source": "neuron",
            "target": target,
            "body": format!("[Neuron] {text}"),
            "outcome": "reminder",
            "reason": "stalled task reminder",
        }),
    );
}

/// `12 dakikadır` — or seconds under the test override's short thresholds.
fn since(age: Duration) -> String {
    match age.as_secs() {
        s if s < 60 => format!("{s} saniyedir"),
        s => format!("{} dakikadır", s / 60),
    }
}

/// Pure: who is responsible for a stalled task and what to tell them.
/// `source_agent` is the agent owning the stalled entry's pane (`None`
/// for a rework wait, which is the coordinator's). `AwaitingReview`
/// entries are keyed by the BUILDER's pane, so the responsible agent is
/// the builder's paired reviewer.
pub(super) fn reminder_for(
    source_agent: Option<&str>,
    stall: &Stall,
) -> Option<(&'static str, String)> {
    let id = &stall.task_id;
    let since = since(stall.age);
    match stall.state {
        LifecycleState::AwaitingReview => {
            let reviewer = reviewer_for_builder(source_agent?)?;
            Some((
                reviewer,
                format!(
                    "hatırlatma: `review {id}` {since} cevapsız. İncelemeyi \
                     bitirip coordinator'a `APPROVED {id}` ya da `CHANGES_NEEDED {id} — \
                     <ne değişmeli>` yolla; takıldıysan coordinator'a `hata — <sebep>` yaz \
                     (task_id alanı `{id}`)."
                ),
            ))
        }
        LifecycleState::Assigned | LifecycleState::Building => {
            let agent = AGENT_IDS.iter().copied().find(|a| Some(*a) == source_agent)?;
            let text = if agent == TESTER {
                format!(
                    "hatırlatma: `{id}` test görevi {since} sonuçsuz. Testi \
                     koşturup coordinator'a `APPROVED {id}` (PASS) ya da `CHANGES_NEEDED \
                     {id} — <hangi test, hangi dosya, hangi builder>` (FAIL) yolla; \
                     koşturamıyorsan coordinator'a `hata — <sebep>` yaz (task_id alanı `{id}`)."
                )
            } else {
                format!(
                    "hatırlatma: `{id}` görevi {since} bitmedi. Bittiyse \
                     coordinator'a `DONE {id} — <değişen dosyalar>` yolla; takıldıysan \
                     coordinator'a `hata — <sebep>` yaz (task_id alanı `{id}`)."
                )
            };
            Some((agent, text))
        }
        LifecycleState::Failed => Some((
            "coordinator",
            format!(
                "hatırlatma: `{id}` için CHANGES_NEEDED geldi ama {since} yeniden \
                 dispatch edilmedi. Reddedenin feedback'iyle ilgili builder'a AYNI task_id \
                 (`{id}`) ile düzeltme dispatch'i yaz (en fazla {MAX_REWORK_ROUNDS} tur); \
                 düzeltilemiyorsa orchestrator'a `TASK_FAILED {id} — <sebep>` yolla."
            ),
        )),
        _ => None,
    }
}

/// Pure: the `TASK_FAILED` body for the orchestrator and the notice for
/// the coordinator when a stalled task is given up.
pub(super) fn give_up_texts(responsible: &str, stall: &Stall) -> (String, String) {
    let id = &stall.task_id;
    let since = since(stall.age);
    (
        format!(
            "{TASK_FAILED_PREFIX}{id} — @{responsible} {since} yanıt vermedi \
             (hatırlatma ve eskalasyondan sonra da)"
        ),
        format!(
            "`{id}` @{responsible}'dan yanıt alınamadığı için kapatıldı; orchestrator'a \
             `TASK_FAILED {id}` iletildi. Bu task_id ile yeniden dispatch ETME."
        ),
    )
}

/// Pure: the level-2 notice — `(recipient, text)`. A stuck specialist
/// is escalated to the coordinator; a stuck coordinator (unsent rework)
/// to the orchestrator.
pub(super) fn escalation_for(responsible: &str, stall: &Stall) -> (&'static str, String) {
    let id = &stall.task_id;
    let since = since(stall.age);
    if responsible == "coordinator" {
        return (
            "orchestrator",
            format!(
                "`{id}` reddedildi ve {since} yeniden dispatch edilmedi; \
                 coordinator hatırlatmaya cevap vermedi. Düzeltmeyi ilgili builder'a \
                 kendin dispatch et (aynı task_id) ya da görevi başarısız say."
            ),
        );
    }
    (
        "coordinator",
        format!(
            "`{id}` görevi {since} @{responsible}'da takılı ({:?}); hatırlatmaya \
             cevap gelmedi. Gerekirse yeniden dispatch et ya da orchestrator'a \
             `TASK_FAILED {id} — <özet>` yolla (task_id alanı `{id}`).",
            stall.state,
        ),
    )
}

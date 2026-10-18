//! Lifecycle synthesis — the bridge's side of the task-lifecycle
//! contract. [`lifecycle_synthesise`] runs on every successful
//! coordinator-bound delivery: it parses the body for a lifecycle token
//! and, when it warrants a fanout, drops the synthesised follow-up
//! envelope into the coordinator's outbox for the watcher to pick up on
//! its next tick. The other helpers keep the store in step with
//! dispatches ([`note_dispatch`], [`dispatch_conflict`]) and with the
//! coordinator's own `TASK_FAILED` reports ([`note_coordinator_report`]).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::swarm_term::hierarchy::{
    builders_judged_by, is_allowed, is_builder, is_dispatcher, is_reviewer,
};
use crate::swarm_term::lifecycle::{
    body_task_id, error_reason, followup_for_coordinator_inbound, is_error_report,
    lifecycle_note, looks_like_unparsed_signal, parse_lifecycle_token_with_fallback,
    task_failed_id, Abandon, Closure, LifecycleState, LifecycleStore, TransitionKind,
    MAX_REWORK_ROUNDS, TASK_FAILED_PREFIX, TESTER,
};

use super::envelope::Envelope;
use super::fsops::drop_envelope;

/// A `[Neuron]` note the watcher pastes into `to`'s pane after a
/// lifecycle step (an unrecognised signal, a closed task, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Notice {
    pub to: String,
    pub text: String,
    pub reason: &'static str,
}

impl Notice {
    fn new(to: &str, text: String, reason: &'static str) -> Self {
        Self { to: to.to_string(), text, reason }
    }
}

/// On a successful coordinator-bound delivery, parse the body for a
/// lifecycle token, record it, and drop the synthesised follow-up (if
/// any) into the coordinator's outbox. The watcher picks it up on its
/// next tick — same delivery path as a user-authored route.
///
/// Returns the notices to paste: to the SENDER when its body looks like
/// a lifecycle signal but did not parse (a malformed `DONE` must not
/// stall the chain silently) or targets a closed task; to the
/// COORDINATOR when the backend closed a task as failed, so it does not
/// re-dispatch it.
pub(super) fn lifecycle_synthesise<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    from_agent: &str,
    body: &str,
    task_id: Option<&str>,
    panes_by_agent: &Arc<HashMap<String, String>>,
    lifecycle: &Arc<LifecycleStore>,
) -> Vec<Notice> {
    let Some(source_pane) = panes_by_agent.get(from_agent).cloned() else {
        return Vec::new();
    };
    // The reporter's verdicts / `hata` reach the builders it judges.
    let judged: Vec<&str> = builders_judged_by(from_agent)
        .iter()
        .filter_map(|b| panes_by_agent.get(*b).map(String::as_str))
        .collect();

    // `hata — …` with a task_id: the reporter gave up on that task.
    // Every outcome is resolved — a `hata` nobody acts on left the
    // orchestrator waiting for the task forever.
    if let Some(id) = task_id.filter(|_| is_error_report(body)) {
        let mut closable = vec![source_pane.as_str()];
        closable.extend(&judged);
        return match lifecycle.abandon(&closable, id) {
            Abandon::Closed | Abandon::Untracked => {
                tracing::info!(
                    source = %from_agent,
                    task_id = %id,
                    "swarm-term bridge: task reported failed (hata) — closed"
                );
                let owners = task_owners(lifecycle, panes_by_agent, id);
                emit_lifecycle(app, from_agent, &source_pane, id, "Abandoned", LifecycleState::Failed, &owners);
                let why = error_reason(body).unwrap_or_else(|| "sebep belirtilmedi".into());
                enqueue_fanout(
                    app,
                    root,
                    panes_by_agent,
                    "orchestrator",
                    format!("{TASK_FAILED_PREFIX}{id} — @{from_agent} hata bildirdi: {why}"),
                    id,
                );
                vec![Notice::new(
                    "coordinator",
                    format!(
                        "`{id}` @{from_agent}'in `hata` bildirimiyle kapandı; orchestrator'a \
                         `TASK_FAILED {id}` iletildi. Bu task_id ile yeniden dispatch ETME — \
                         orchestrator karar verecek."
                    ),
                    "task closed after hata",
                )]
            }
            Abandon::Elsewhere => vec![Notice::new(
                "coordinator",
                format!(
                    "@{from_agent} `{id}` için `hata` bildirdi ama görev başka bir ajanda \
                     hâlâ açık, bu yüzden kapatılmadı. Görev gerçekten yürümüyorsa \
                     orchestrator'a `TASK_FAILED {id} — <sebep>` yolla (task_id alanı `{id}`)."
                ),
                "hata about a task open elsewhere",
            )],
            Abandon::AlreadyClosed(closure) => {
                vec![closed_notice(from_agent, id, closure)]
            }
        };
    }

    let Some(transition) = parse_lifecycle_token_with_fallback(body, task_id) else {
        return unparsed_signal_notice(from_agent, body, task_id.is_some())
            .map(|text| {
                Notice::new(from_agent, text, "lifecycle signal not recognised; no auto-fanout")
            })
            .into_iter()
            .collect();
    };
    let id = transition.task_id.clone();
    // The sender's free-text note (what changed / what to check / why it
    // was rejected) — carried into the follow-up so it is not
    // context-free.
    let note = lifecycle_note(body, &id);
    let rec = lifecycle.record(from_agent, &source_pane, &transition, &judged, note.as_deref());
    tracing::info!(
        source = %from_agent,
        source_pane = %source_pane,
        task_id = %id,
        transition = ?transition.kind,
        new_state = ?rec.state,
        rejections = rec.rejections,
        retest = rec.retest,
        "swarm-term bridge: lifecycle transition recorded"
    );
    if let Some(closure) = rec.already_closed {
        return vec![closed_notice(from_agent, &id, closure)];
    }
    // A CHANGES_NEEDED that used up the rework limit CLOSES the task as
    // failed; both it and a normal within-limit rejection record
    // `state: Failed` (lifecycle::apply_transition), so label the closing
    // one "Failed" — otherwise the UI shows it as "rework" (a fix coming)
    // instead of "failed".
    let transition_label = if rec.limit_exceeded {
        "Failed".to_string()
    } else {
        format!("{:?}", transition.kind)
    };
    emit_lifecycle(
        app,
        from_agent,
        &source_pane,
        &id,
        &transition_label,
        rec.state,
        &task_owners(lifecycle, panes_by_agent, &id),
    );

    let Some((target, base)) = followup_for_coordinator_inbound(from_agent, &transition, &rec)
    else {
        return Vec::new();
    };
    // A retest carries the BUILDERS' notes (what the fix changed), not
    // the reviewer's approval text; the TASK_DONE that closes such a task
    // appends them, so the orchestrator's report lists the rework.
    let (note, fix) = if rec.retest {
        (rec.builder_note.clone(), None)
    } else {
        (note, rec.builder_note.clone())
    };
    let mut synth_body = match note {
        Some(note) => format!("{base} — {note}"),
        None => base,
    };
    if let Some(fix) = fix {
        synth_body.push_str(&format!(" (düzeltme: {fix})"));
    }
    enqueue_fanout(app, root, panes_by_agent, &target, synth_body, &id);
    match transition.kind {
        TransitionKind::Approved => {
            lifecycle.mark_done(&source_pane, &id);
            Vec::new()
        }
        TransitionKind::ChangesNeeded if rec.limit_exceeded => vec![Notice::new(
            "coordinator",
            format!(
                "`{id}` {}. kez reddedildi; düzeltme limiti ({MAX_REWORK_ROUNDS} tur) aşıldı. \
                 Görev kapatıldı ve orchestrator'a `TASK_FAILED {id}` iletildi — yeniden \
                 dispatch ETME.",
                rec.rejections
            ),
            "rework limit exceeded",
        )],
        _ => Vec::new(),
    }
}

/// A dispatcher (orchestrator / coordinator) delivered a `task_id`-
/// bearing message to a builder or the tester: track it as `Assigned`,
/// so a dispatch nobody acts on stalls instead of waiting forever.
pub(super) fn note_dispatch<R: Runtime>(
    app: &AppHandle<R>,
    from_agent: &str,
    to_agent: &str,
    target_pane: &str,
    task_id: Option<&str>,
    lifecycle: &LifecycleStore,
) {
    let Some(id) = task_id.filter(|_| tracks_dispatch(from_agent, to_agent)) else {
        return;
    };
    if lifecycle.assign(target_pane, id) {
        emit_lifecycle(
            app,
            to_agent,
            target_pane,
            id,
            "Assigned",
            LifecycleState::Assigned,
            &[to_agent.to_string()],
        );
    }
}

/// Why a dispatch must NOT be delivered (a closed task id, or an id
/// still open on the other builder), or `None`. Checked before delivery.
pub(super) fn dispatch_conflict(
    from_agent: &str,
    to_agent: &str,
    target_pane: &str,
    task_id: Option<&str>,
    panes_by_agent: &HashMap<String, String>,
    lifecycle: &LifecycleStore,
) -> Option<String> {
    let id = task_id.filter(|_| tracks_dispatch(from_agent, to_agent))?;
    let builders: Vec<(&str, &str)> = panes_by_agent
        .iter()
        .filter(|(agent, _)| is_builder(agent))
        .map(|(agent, pane)| (agent.as_str(), pane.as_str()))
        .collect();
    lifecycle.dispatch_conflict(target_pane, id, &builders)
}

fn tracks_dispatch(from_agent: &str, to_agent: &str) -> bool {
    is_dispatcher(from_agent) && (is_builder(to_agent) || to_agent == TESTER)
}

/// The task id a delivery carries: the envelope field, or — for a
/// tracked dispatch whose sender wrote `task_id=<id>` only in the body —
/// that marker. Without the fallback such a dispatch was never tracked,
/// and the receiver's later `hata` closed nothing.
pub(super) fn delivery_task_id(
    from_agent: &str,
    to_agent: &str,
    body: &str,
    field: Option<String>,
) -> Option<String> {
    field.or_else(|| {
        tracks_dispatch(from_agent, to_agent)
            .then(|| body_task_id(body).map(str::to_string))
            .flatten()
    })
}

fn closed_notice(to: &str, id: &str, closure: Closure) -> Notice {
    let how = match closure {
        Closure::Done => "TASK_DONE",
        Closure::Failed => "TASK_FAILED",
    };
    Notice::new(
        to,
        format!(
            "`{id}` görevi zaten kapandı ({how}); bu sinyal işlenmedi, otomatik zincir \
             başlamadı. Yeni bir iş ise yeni bir task_id ile gelmesi gerekir."
        ),
        "task already closed",
    )
}

/// The coordinator told the orchestrator `TASK_FAILED <id>` (it gave up
/// on a task): close it so it stops stalling and cannot be re-dispatched.
pub(super) fn note_coordinator_report<R: Runtime>(
    app: &AppHandle<R>,
    from_agent: &str,
    to_agent: &str,
    body: &str,
    task_id: Option<&str>,
    panes_by_agent: &HashMap<String, String>,
    lifecycle: &LifecycleStore,
) {
    if from_agent != "coordinator" || to_agent != "orchestrator" {
        return;
    }
    let Some(id) = task_failed_id(body, task_id) else {
        return;
    };
    if lifecycle.fail_task(id) {
        tracing::info!(task_id = %id, "swarm-term bridge: coordinator gave up a task — closed");
        let pane = panes_by_agent.get("coordinator").map(String::as_str).unwrap_or("");
        let owners = task_owners(lifecycle, panes_by_agent, id);
        emit_lifecycle(app, "coordinator", pane, id, "TaskFailed", LifecycleState::Failed, &owners);
    }
}

/// The agents whose work `task_id` is: the panes holding it, minus the
/// reviewers / tester that only judged it — unless nobody else holds it
/// (the tester's own smoke task). Lets the UI show a reviewer's
/// `CHANGES_NEEDED` on the BUILDER, not on the reviewer.
pub(super) fn task_owners(
    lifecycle: &LifecycleStore,
    panes_by_agent: &HashMap<String, String>,
    task_id: &str,
) -> Vec<String> {
    let mut agents: Vec<String> = lifecycle
        .holders(task_id)
        .iter()
        .filter_map(|pane| {
            panes_by_agent
                .iter()
                .find(|(_, p)| *p == pane)
                .map(|(agent, _)| agent.clone())
        })
        .collect();
    agents.sort();
    let workers: Vec<String> = agents.iter().filter(|a| !is_reviewer(a)).cloned().collect();
    if workers.is_empty() {
        agents
    } else {
        workers
    }
}

fn emit_lifecycle<R: Runtime>(
    app: &AppHandle<R>,
    source: &str,
    source_pane: &str,
    task_id: &str,
    transition: &str,
    state: LifecycleState,
    owners: &[String],
) {
    let _ = app.emit(
        crate::events::SWARM_TERM_LIFECYCLE,
        json!({
            "source": source,
            "source_pane": source_pane,
            "task_id": task_id,
            "transition": transition,
            "state": format!("{state:?}"),
            "owners": owners,
        }),
    );
}

/// Drop a coordinator-signed follow-up envelope for `target`.
pub(super) fn enqueue_fanout<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    panes_by_agent: &HashMap<String, String>,
    target: &str,
    body: String,
    task_id: &str,
) {
    if !is_allowed("coordinator", target) || !panes_by_agent.contains_key(target) {
        tracing::warn!(
            synth_target = %target,
            "swarm-term bridge: lifecycle fanout dropped — target not reachable"
        );
        return;
    }
    let env = Envelope {
        from: "coordinator".into(),
        to: target.to_string(),
        body: body.clone(),
        task_id: Some(task_id.to_string()),
    };
    match drop_envelope(root, &env) {
        Ok(_) => {
            tracing::info!(
                to = %target,
                body = %body,
                "swarm-term bridge: lifecycle auto-fanout enqueued"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": "coordinator",
                    "target": target,
                    "body": body,
                    "outcome": "lifecycle_fanout",
                }),
            );
        }
        Err(e) => tracing::warn!(
            target = %target,
            error = %e,
            "swarm-term bridge: failed to write synth envelope"
        ),
    }
}

/// Notice for a builder/reviewer whose coordinator-bound body looks
/// like a lifecycle signal but did not parse. `None` for other roles
/// and for ordinary prose.
fn unparsed_signal_notice(
    from_agent: &str,
    body: &str,
    has_task_id: bool,
) -> Option<String> {
    let expected = if is_builder(from_agent) {
        "Görev bittiyse body TAM OLARAK `DONE <task_id> — <ne değişti, hangi \
         dosyalar>` biçiminde olmalı (ör. `DONE T1 — auth.rs: refresh eklendi`)"
    } else if is_reviewer(from_agent) {
        "Karar verdiysen body TAM OLARAK `APPROVED <task_id>` ya da \
         `CHANGES_NEEDED <task_id> — <ne değişmeli>` biçiminde olmalı"
    } else {
        return None;
    };
    if !looks_like_unparsed_signal(body, has_task_id) {
        return None;
    }
    Some(format!(
        "@coordinator'a gönderdiğin mesaj bir görev sinyali olarak TANINMADI; \
         otomatik zincir (review / TASK_DONE) BAŞLAMADI.\n{expected}. \
         task_id alanını da doldur ve yeni bir dosyayla tekrar gönder."
    ))
}

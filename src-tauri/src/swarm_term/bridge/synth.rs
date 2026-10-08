//! Lifecycle synthesis — runs only on a successful coordinator-bound
//! delivery. Parses the body for a lifecycle token and, when it warrants
//! a fanout, drops a synthesised follow-up envelope into the appropriate
//! inbox for the watcher to pick up on its next tick.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use tauri::{AppHandle, Emitter, Runtime};

use crate::swarm_term::hierarchy::{is_allowed, is_builder, is_reviewer};
use crate::swarm_term::lifecycle::{
    followup_for_coordinator_inbound, lifecycle_note,
    looks_like_unparsed_signal, parse_lifecycle_token_with_fallback,
    LifecycleStore, TransitionKind,
};

use super::envelope::Envelope;
use super::fsops::drop_envelope;

/// On a successful coordinator-bound delivery, parse the body for a
/// lifecycle token and, if it's one that warrants a fanout, drop the
/// synthesised follow-up envelope into the appropriate inbox. The
/// watcher picks it up on its next tick — same delivery path as a
/// user-authored route.
///
/// Returns a notice for the SENDER when a builder/reviewer body looks
/// like a lifecycle signal but did not parse — the caller pastes it
/// into the sender's pane so a malformed `DONE` does not stall the
/// chain silently.
pub(super) fn lifecycle_synthesise<R: Runtime>(
    app: &AppHandle<R>,
    root: &Path,
    from_agent: &str,
    body: &str,
    task_id: Option<&str>,
    panes_by_agent: &Arc<HashMap<String, String>>,
    lifecycle: &Arc<LifecycleStore>,
) -> Option<String> {
    let Some(transition) = parse_lifecycle_token_with_fallback(body, task_id)
    else {
        return unparsed_signal_notice(from_agent, body, task_id.is_some());
    };
    let source_pane = panes_by_agent.get(from_agent).cloned()?;
    let new_state = lifecycle.record(&source_pane, &transition);
    tracing::info!(
        source = %from_agent,
        source_pane = %source_pane,
        task_id = %transition.task_id,
        transition = ?transition.kind,
        new_state = ?new_state,
        "swarm-term bridge: lifecycle transition recorded"
    );
    let _ = app.emit(
        crate::events::SWARM_TERM_LIFECYCLE,
        json!({
            "source": from_agent,
            "source_pane": source_pane,
            "task_id": transition.task_id,
            "transition": format!("{:?}", transition.kind),
            "state": format!("{new_state:?}"),
        }),
    );

    let (synth_target, synth_body) =
        followup_for_coordinator_inbound(from_agent, &transition)?;
    // Carry the sender's free-text note (what changed / what to check)
    // so the reviewer does not receive a context-free `review <id>`.
    let synth_body = match lifecycle_note(body, &transition.task_id) {
        Some(note) => format!("{synth_body} — {note}"),
        None => synth_body,
    };
    if !is_allowed("coordinator", &synth_target) {
        tracing::warn!(
            source = %from_agent,
            synth_target = %synth_target,
            "swarm-term bridge: lifecycle fanout dropped — hierarchy forbids coordinator→{}",
            synth_target,
        );
        return None;
    }
    if !panes_by_agent.contains_key(&synth_target) {
        tracing::warn!(
            source = %from_agent,
            synth_target = %synth_target,
            "swarm-term bridge: lifecycle fanout dropped — no pane for synth target"
        );
        return None;
    }
    let env = Envelope {
        from: "coordinator".into(),
        to: synth_target.clone(),
        body: synth_body.clone(),
        task_id: Some(transition.task_id.clone()),
    };
    match drop_envelope(root, &env) {
        Ok(_) => {
            tracing::info!(
                from = "coordinator",
                to = %synth_target,
                body = %synth_body,
                "swarm-term bridge: lifecycle auto-fanout enqueued"
            );
            let _ = app.emit(
                crate::events::SWARM_TERM_ROUTE,
                json!({
                    "source": "coordinator",
                    "target": synth_target,
                    "body": synth_body,
                    "outcome": "lifecycle_fanout",
                }),
            );
        }
        Err(e) => {
            tracing::warn!(
                target = %synth_target,
                error = %e,
                "swarm-term bridge: failed to write synth envelope"
            );
        }
    }
    if matches!(transition.kind, TransitionKind::Approved) {
        lifecycle.mark_done(&source_pane, &transition.task_id);
    }
    None
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

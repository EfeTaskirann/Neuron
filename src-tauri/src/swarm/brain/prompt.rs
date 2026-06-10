//! Per-turn prompt rendering + the summary truncator.
//!
//! Split out of the monolithic `brain.rs` (WP-W5-03). The prompt
//! bodies are Turkish (matching the Coordinator persona's working
//! language) so the persona-tuned prompt stays one-language. Text
//! is verbatim from the pre-split module.

use crate::swarm::mailbox_bus::MailboxEvent;

/// Initial prompt rendered from the user's goal. The body is
/// Turkish (matching the Coordinator persona's working language)
/// so the persona-tuned prompt stays one-language.
pub(super) fn render_initial_prompt(goal: &str) -> String {
    format!(
        "GOAL: {goal}\n\n\
         Sen Coordinator brain'sin (W5-03 dispatch protocol). Bu \
         hedefi tamamlamak için adım adım dispatch kararları ver. \
         Her turn'da TAM OLARAK bir JSON action emit et:\n\n\
         - `dispatch` — bir specialist'e (scout, planner, backend-builder, \
           backend-reviewer, frontend-builder, frontend-reviewer, \
           integration-tester) sub-task gönder. Builder'lar için Plan \
           çıktısını prompt'ta paylaş; reviewer/tester'lar JSON Verdict \
           emit edecek (sen okuyup karar verirsin).\n\
         - `finish` — iş bittiğinde `outcome: \"done\"` veya \
           `outcome: \"failed\"` ile sonlandır.\n\
         - `ask_user` — son çare: kullanıcıdan açıklama gerektiğinde.\n\
         - `help_outcome` — bir specialist'in `neuron_help` block'una \
           cevap olarak (target = \"agent:<id>\", body_json = \
           CoordinatorHelpOutcome JSON).\n\n\
         OUTPUT CONTRACT — yalnızca tek bir JSON object çıkar:\n\
         ```json\n\
         {{\"action\": \"dispatch\", \"target\": \"agent:scout\", \"prompt\": \"...\", \"with_help_loop\": false}}\n\
         ```",
    )
}

/// Render the next turn's prompt from the consumed mailbox event.
pub(super) fn render_next_turn(event: &MailboxEvent) -> String {
    match event {
        MailboxEvent::AgentResult {
            agent_id,
            assistant_text,
            total_cost_usd,
            turn_count,
            ..
        } => {
            format!(
                "Specialist `{agent_id}` finished a task ({turn_count} turns, \
                 ${total_cost_usd:.4}).\n\n\
                 RESULT:\n{assistant_text}\n\n\
                 Bir sonraki action'ı emit et (dispatch / finish / ask_user / help_outcome)."
            )
        }
        MailboxEvent::AgentHelpRequest {
            agent_id,
            reason,
            question,
            ..
        } => {
            format!(
                "Specialist `{agent_id}` bir blocker'a takıldı ve yardım \
                 istiyor.\n\nREASON: {reason}\nQUESTION: {question}\n\n\
                 `help_outcome` action'ı emit et — target = \"agent:{agent_id}\", \
                 body_json = serialised CoordinatorHelpOutcome \
                 (`{{\"action\":\"direct_answer\",\"answer\":\"...\"}}` veya \
                 `{{\"action\":\"ask_back\",\"followup_question\":\"...\"}}` veya \
                 `{{\"action\":\"escalate\",\"user_question\":\"...\"}}`)."
            )
        }
        MailboxEvent::JobCancel { .. } => {
            // Cancel is handled by the loop's select branch, not by
            // re-rendering. Defensive: if this somehow lands as a
            // "next turn" event, stop the brain with a cancel-shaped
            // prompt (the loop's select will catch the actual cancel
            // signal on the next iteration).
            "Job cancelled by user. Emit `finish` with outcome \"failed\".".into()
        }
        // The brain filters out other variants in
        // `envelope_is_relevant`; defensive default.
        other => {
            format!(
                "Unexpected mailbox event reached the brain loop \
                 (kind={}). Emit a `finish` action with outcome \
                 \"failed\" and a summary.",
                other.kind_str()
            )
        }
    }
}

/// Prompt rendered after a `help_outcome` is emitted. Re-asks the
/// coordinator for the next dispatch so the loop continues.
pub(super) fn render_after_help_outcome() -> String {
    "help_outcome was delivered to the specialist. \
     Bir sonraki action'ı emit et — specialist'in cevabı \
     ileride bir AgentResult olarak gelecek; şimdi paralel \
     bir dispatch atabilirsin VEYA `finish` ile bitirebilirsin."
        .into()
}

// Re-export the shared summary truncation so `brain::mod`'s
// `use prompt::truncate_for_summary` path keeps resolving; the
// implementation lives in `crate::text` (shared with
// `commands::swarm::dispatch`).
pub(super) use crate::text::truncate_for_summary;

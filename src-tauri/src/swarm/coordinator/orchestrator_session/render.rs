//! Prompt assembly: prepend recent chat history before the new user
//! message so the persona sees prior context. See the [module
//! docs](super) for where the IPC handler drives this.

use crate::swarm::coordinator::orchestrator::{
    OrchestratorAction, OrchestratorOutcome,
};

use super::model::{OrchestratorMessage, OrchestratorMessageRole};

/// Prompt template the IPC handler uses to inject recent history
/// into the next decide call. `{history_lines}` is one line per
/// prior message; `{user_message}` is the current user input.
///
/// Turkish surface text mirrors the `orchestrator.md` persona
/// language so the LLM sees consistent register across the system
/// prompt and the runtime context.
const HISTORY_TEMPLATE: &str = "Önceki konuşma (eskiden yeniye):\n\n\
{history_lines}\n\n\
---\n\nKullanıcının yeni mesajı:\n\n{user_message}\n";

/// Render a context-aware prompt that prepends `history` (chronological,
/// oldest-first) before `user_message`. When `history` is empty the
/// result is `user_message.to_string()` verbatim — no header, no
/// separator — so the very first turn is byte-identical to the W3-12k1
/// stateless behaviour.
///
/// Per-message formatting:
///
/// - `User` rows: `[user]: <content>`
/// - `Orchestrator` rows: `[orchestrator/<action>]: <text>` (decoded
///   from the JSON-packed outcome). If decode fails, the row is
///   surfaced as `[orchestrator]: <raw content>` so the prompt is
///   never silently dropped.
/// - `Job` rows: `[swarm dispatched]: <job_id> (goal: <goal>)`. A
///   missing goal column renders as `(goal: -)`.
///
/// All formatting is line-grain — newlines inside `content` would
/// confuse the LLM about which line is which message. Practically:
/// user messages and orchestrator text are short conversational
/// strings; if they ever grow paragraphs we revisit this in W3-12k4.
pub(crate) fn render_with_history(
    history: &[OrchestratorMessage],
    user_message: &str,
) -> String {
    if history.is_empty() {
        return user_message.to_string();
    }
    let history_lines: Vec<String> = history
        .iter()
        .map(|m| match m.role {
            OrchestratorMessageRole::User => {
                format!("[user]: {}", m.content)
            }
            OrchestratorMessageRole::Orchestrator => {
                // Decode the JSON-packed outcome for human-readable
                // display. A parse failure surfaces the raw content
                // rather than panicking — the prompt is informational
                // (the LLM tolerates noise), not a contract.
                match serde_json::from_str::<OrchestratorOutcome>(&m.content) {
                    Ok(outcome) => format!(
                        "[orchestrator/{}]: {}",
                        action_label(outcome.action),
                        outcome.text,
                    ),
                    Err(_) => format!("[orchestrator]: {}", m.content),
                }
            }
            OrchestratorMessageRole::Job => format!(
                "[swarm dispatched]: {} (goal: {})",
                m.content,
                m.goal.as_deref().unwrap_or("-"),
            ),
        })
        .collect();
    HISTORY_TEMPLATE
        .replace("{history_lines}", &history_lines.join("\n"))
        .replace("{user_message}", user_message)
}

/// One-line label for an `OrchestratorAction` used in
/// [`render_with_history`]. Mirrors the snake_case wire form so the
/// prompt the LLM reads matches the OUTPUT CONTRACT it must emit.
fn action_label(action: OrchestratorAction) -> &'static str {
    match action {
        OrchestratorAction::DirectReply => "direct_reply",
        OrchestratorAction::Clarify => "clarify",
        OrchestratorAction::Dispatch => "dispatch",
    }
}

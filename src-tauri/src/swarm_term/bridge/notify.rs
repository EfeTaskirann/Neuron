//! Sender-facing notices. When the bridge permanently gives up on an
//! envelope — or delivers one whose lifecycle signal it could not
//! parse — it tells the AGENT that wrote it, not just the UI Routing
//! Log. Without this the sender believes the message went out and
//! waits for a reply that can never come.
//!
//! Notices are pasted straight into the sender's pane (they are not
//! envelopes), so they can never loop back through the bridge.

use std::collections::HashMap;
use std::time::Duration;

use crate::sidecar::terminal::TerminalRegistry;
use crate::swarm_term::hierarchy::allowed_for;

use super::delivery::{
    format_system_notice, paste_then_submit, truncate, WRITE_TIMEOUT_SECS,
};

/// Best-effort paste of a `[Neuron] …` notice into `agent`'s pane.
/// Failures are logged and swallowed — a notice must never block or
/// fail the delivery pipeline that produced it.
pub(crate) async fn notify_agent(
    registry: &TerminalRegistry,
    panes_by_agent: &HashMap<String, String>,
    agent: &str,
    text: &str,
) {
    let Some(pane) = panes_by_agent.get(agent) else {
        return;
    };
    let bytes = format_system_notice(text).into_bytes();
    match tokio::time::timeout(
        Duration::from_secs(WRITE_TIMEOUT_SECS),
        paste_then_submit(registry, pane, &bytes),
    )
    .await
    {
        Ok(Ok(())) => tracing::info!(
            agent = %agent,
            "swarm-term bridge: sender notice delivered"
        ),
        Ok(Err(e)) => tracing::warn!(
            agent = %agent,
            error = %e,
            "swarm-term bridge: sender notice write failed"
        ),
        Err(_) => tracing::warn!(
            agent = %agent,
            "swarm-term bridge: sender notice write timed out"
        ),
    }
}

/// Text of the notice sent when `sender`'s message to `target` was
/// moved to `rejected/`. Carries the reason, a body excerpt (so the
/// agent knows WHICH message failed) and its allowed destinations (so a
/// hierarchy denial is immediately actionable).
pub(super) fn rejection_notice(
    sender: &str,
    target: &str,
    reason: &str,
    body: &str,
) -> String {
    let allowed = allowed_for(sender)
        .iter()
        .map(|a| format!("@{a}"))
        .collect::<Vec<_>>()
        .join(", ");
    let excerpt = truncate(body.trim(), 160);
    format!(
        "@{target} hedefine yazdığın mesaj İLETİLEMEDİ ve iptal edildi.\n\
         Sebep: {reason}\n\
         Mesaj: \"{excerpt}\"\n\
         İzinli hedeflerin: {allowed}\n\
         Gerekiyorsa düzeltip YENİ bir dosya adıyla tekrar gönder; \
         izinli değilse işi @coordinator üzerinden yönlendir."
    )
}

//! Tests for the job projector. Split from the former single
//! `projector/tests.rs` (1195L) into per-concern submodules
//! (T3-26, DEEP); the test bodies are moved verbatim (one dedent
//! level only — see the prior `mod tests {}` indentation).
//!
//! `mod.rs` keeps only the shared event-capture harness
//! (`CapturedEvent` / `install_event_capturer` / `drain_until`),
//! which the `emit` integration tests consume; every submodule
//! carries its own explicit imports.

use super::*;
use crate::events;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tauri::Listener;

mod emit;
mod migration;
mod outcome;
mod pure;

/// One captured `swarm:job:{id}:event` payload. `kind` is the
/// top-level `kind` tag from `SwarmJobEvent`'s
/// `#[serde(tag = "kind")]`; `json` is the full payload Value
/// so individual assertions can dig into per-variant fields.
/// `SwarmJobEvent` is `Serialize`-only (no Deserialize), which
/// is why we capture as `Value` rather than the typed enum.
#[derive(Debug, Clone)]
struct CapturedEvent {
    kind: String,
    json: serde_json::Value,
}

/// Capture every `swarm:job:{job_id}:event` payload in
/// chronological order. Mirrors the FSM tests' `capture_events`
/// helper but scoped to this module so we don't reach into
/// `coordinator`.
fn install_event_capturer<R: Runtime>(
    app: &tauri::App<R>,
    job_id: &str,
) -> Arc<StdMutex<Vec<CapturedEvent>>> {
    let captured: Arc<StdMutex<Vec<CapturedEvent>>> =
        Arc::new(StdMutex::new(Vec::new()));
    let captured_w = Arc::clone(&captured);
    app.listen(events::swarm_job_event(job_id), move |event| {
        let payload = event.payload().to_string();
        if let Ok(value) =
            serde_json::from_str::<serde_json::Value>(&payload)
        {
            let kind = value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            captured_w
                .lock()
                .expect("lock")
                .push(CapturedEvent { kind, json: value });
        }
    });
    captured
}

/// Wait until the captured events match a predicate, polling
/// on 20 ms ticks. Bounded so a regression surfaces as a test
/// failure rather than a hang.
async fn drain_until<F: Fn(&[CapturedEvent]) -> bool>(
    captured: &Arc<StdMutex<Vec<CapturedEvent>>>,
    pred: F,
) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let snap = captured.lock().expect("lock").clone();
        if pred(&snap) {
            return;
        }
        if std::time::Instant::now() > deadline {
            let snap = captured.lock().expect("lock").clone();
            panic!(
                "timeout waiting for predicate; captured kinds: {:?}",
                snap.iter().map(|e| e.kind.clone()).collect::<Vec<_>>()
            );
        }
    }
}

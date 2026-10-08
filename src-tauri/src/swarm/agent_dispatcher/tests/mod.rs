//! Dispatcher unit coverage, split per concern from the single-file
//! `tests.rs` (T3-27, 2026-06-16): target parsing (`target`), the
//! routing / cancel / lagged-receiver / shutdown matrix (`routing`),
//! and the WP-W5-03 help-loop branch (`help_loop`). All tests run
//! against a real `MailboxBus` with a closure-based mock of the
//! `AgentInvoker` seam so no `claude` subprocess is spawned. Only the
//! module wrapping changed; every assertion is the byte-for-byte
//! pre-split version.
//!
//! The one cross-module helper, [`wait_for_envelope`], lives here and
//! is reached by the submodules via `super::wait_for_envelope`. Each
//! submodule owns the invoker mock it needs (`MockAgentInvoker` in
//! `routing`, `ScriptedInvoker` in `help_loop`).

use super::*;
use std::time::Duration as StdDuration;
use tokio::time::sleep;

mod help_loop;
mod routing;
mod target;

/// Helper: poll the bus's mailbox table for a row matching the
/// predicate, with a soft timeout so the test fails fast rather
/// than hanging if the dispatcher never emits.
async fn wait_for_envelope<F>(
    bus: &MailboxBus,
    kind: &str,
    predicate: F,
) -> Option<MailboxEnvelope>
where
    F: Fn(&MailboxEnvelope) -> bool,
{
    let deadline =
        std::time::Instant::now() + StdDuration::from_secs(5);
    loop {
        let rows = bus
            .list_typed(Some(kind), None, Some(500))
            .await
            .expect("list ok");
        if let Some(env) =
            rows.into_iter().find(|env| predicate(env))
        {
            return Some(env);
        }
        if std::time::Instant::now() > deadline {
            return None;
        }
        sleep(StdDuration::from_millis(20)).await;
    }
}

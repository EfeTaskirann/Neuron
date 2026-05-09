---
id: WP-W5-02
title: Agent mailbox subscription + auto-emit (MailboxAgentDispatcher per agent)
owner: TBD
status: not-started
depends-on: [WP-W5-01]
acceptance-gate: "New `MailboxAgentDispatcher` per (workspace, agent) pair, spawned by `SwarmAgentRegistry` on first `task_dispatch` event whose `target` matches `agent:<id>`. Dispatcher subscribes to the workspace's `MailboxBus` channel; on `task_dispatch`, calls `acquire_and_invoke_turn` and auto-emits `MailboxEvent::AgentResult` with `parent_id` pointing at the dispatch row. On `job_cancel`, signals the in-flight invoke's `Notify` for graceful turn-truncate. New `swarm:agents:dispatch_to_agent` IPC for tests + manual dispatch. NO change to FSM, RegistryTransport, or W4-05 help-loop wiring (help-loop migration to mailbox events is deferred to W5-03). `cargo test --lib` ≥ 12 new unit tests; `pnpm typecheck` / `lint` / `gen:bindings:check` green."
---

## Goal

Wire each agent session in `SwarmAgentRegistry` to the W5-01
mailbox event-bus so that emitting a `MailboxEvent::TaskDispatch`
event causes the named agent to run a turn — without going
through the FSM. This is the pull-side counterpart to the bus's
push-side: W5-01 made the bus emittable; W5-02 makes agents
listen to it.

The dispatcher task absorbs three responsibilities:

1. **Route** — match incoming `task_dispatch` events against
   `target == agent:<this_agent_id>`; ignore others.
2. **Invoke** — call the existing
   `SwarmAgentRegistry::acquire_and_invoke_turn` with the
   dispatch's prompt + a fresh per-invoke cancel `Notify`.
3. **Emit** — on result, post `MailboxEvent::AgentResult`
   back to the bus with `parent_id` pointing at the dispatch
   row's `id`. The W5-04 projector reads these chains to
   derive job state.

Help-loop migration (turning W4-05's transparent
`acquire_and_invoke_turn_with_help` into mailbox-mediated
`AgentHelpRequest` ↔ `CoordinatorHelpOutcome` round-trips) is
**out of scope** for W5-02. Today's W4-05 substrate keeps
working; W5-02 dispatchers call the non-help variant
(`acquire_and_invoke_turn`) for now, and the W5-03 Coordinator
brain WP migrates the help loop in the same change that
introduces the brain itself.

## Why now

Owner directive 2026-05-09 §1: dispatch should be mailbox-driven.
W5-01 shipped the bus; W5-02 is the next required step before any
mailbox-driven dispatch path can light up.

W5-02 also unlocks a debug surface (`swarm:agents:dispatch_to_agent`)
that lets the orchestrator + tests + dev tools fire a single
agent invoke without involving the FSM — useful for soak-testing
the W5 substrate before the brain (W5-03) lands.

## Charter alignment

- **Tech stack**: no new dependency. Reuses
  `tokio::sync::broadcast::Receiver`, `tokio::sync::Notify`, and
  existing tokio task spawn primitives.
- **Frontend mock shape**: no UI in this WP; no wire-shape change
  to the existing `MailboxEntry` / `MailboxEnvelope` types.
- **Identifier strategy** (ADR-0007): the dispatch event's
  `target` field uses the W4-07 namespacing convention
  (`agent:<id>`); the agent_id is the W3-11 profile id (slug,
  not prefixed-ULID). No new identifier domain.
- **Timestamp invariant** (Charter §8): N/A — no new timestamps.

## Scope

### 1. New module `src-tauri/src/swarm/agent_dispatcher.rs`

```rust
//! `MailboxAgentDispatcher` — per-(workspace, agent) task that
//! consumes `MailboxEvent::TaskDispatch` events from the W5-01
//! `MailboxBus` and routes matching ones to
//! `SwarmAgentRegistry::acquire_and_invoke_turn`.

use std::sync::Arc;
use tauri::{AppHandle, Runtime};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::task::JoinHandle;

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope, MailboxEvent};

/// One dispatcher task per (workspace, agent_id). The task lives
/// for the workspace's lifetime; on workspace shutdown the
/// `JoinHandle` is awaited (or aborted) by the registry.
pub struct MailboxAgentDispatcher {
    workspace_id: String,
    agent_id: String,
    handle: JoinHandle<()>,
    /// Lock-protected slot for the in-flight invoke's cancel
    /// notify. `Some((job_id, notify))` while a turn is running;
    /// `None` while idle. The `JobCancel` arm reads this to
    /// signal the matching in-flight invoke without racing.
    current_invoke: Arc<Mutex<Option<(String, Arc<Notify>)>>>,
}

impl MailboxAgentDispatcher {
    /// Spawn a dispatcher task. Subscribes to the bus's per-
    /// workspace channel and loops on `recv()`. The task owns
    /// the receiver (no shared state with other dispatchers).
    pub fn spawn<R: Runtime>(
        app: AppHandle<R>,
        workspace_id: String,
        agent_id: String,
        registry: Arc<SwarmAgentRegistry>,
        bus: Arc<MailboxBus>,
    ) -> Self;

    /// Tell the dispatcher to stop. Sends a sentinel via a
    /// dedicated shutdown Notify; the task drops the receiver
    /// and exits cleanly. Idempotent.
    pub async fn shutdown(self);
}

/// Resolve `target` strings of the form `agent:<id>` to the bare
/// agent id. Returns `None` if the prefix is missing or empty.
pub fn parse_agent_target(target: &str) -> Option<&str>;
```

The dispatcher's main loop, in pseudocode:

```rust
loop {
    tokio::select! {
        _ = shutdown.notified() => break,
        event = receiver.recv() => match event {
            Ok(envelope) => match &envelope.event {
                MailboxEvent::TaskDispatch { job_id, target, prompt, with_help_loop } => {
                    if parse_agent_target(target) != Some(&agent_id) { continue; }
                    let notify = Arc::new(Notify::new());
                    *current_invoke.lock().await = Some((job_id.clone(), notify.clone()));

                    let result = registry.acquire_and_invoke_turn(
                        &app, &workspace_id, &agent_id, prompt,
                        DEFAULT_DISPATCH_TIMEOUT, notify,
                    ).await;

                    *current_invoke.lock().await = None;

                    let result_event = match result {
                        Ok(invoke_result) => MailboxEvent::AgentResult {
                            job_id: job_id.clone(),
                            agent_id: agent_id.clone(),
                            assistant_text: invoke_result.assistant_text,
                            total_cost_usd: invoke_result.total_cost_usd,
                            turn_count: invoke_result.num_turns,
                        },
                        Err(e) => MailboxEvent::AgentResult {
                            job_id: job_id.clone(),
                            agent_id: agent_id.clone(),
                            assistant_text: format!("error: {e}"),
                            total_cost_usd: 0.0,
                            turn_count: 0,
                        },
                    };
                    bus.emit_typed(
                        &app, &workspace_id,
                        &format!("agent:{agent_id}"),
                        "agent:coordinator",
                        "result",
                        Some(envelope.id),
                        result_event,
                    ).await.ok();
                }
                MailboxEvent::JobCancel { job_id } => {
                    let slot = current_invoke.lock().await;
                    if let Some((active_job, notify)) = slot.as_ref() {
                        if active_job == job_id {
                            notify.notify_one();
                        }
                    }
                }
                _ => {} // ignore other event kinds
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    %workspace_id, %agent_id, %skipped,
                    "agent dispatcher lagged; events skipped"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}
```

`DEFAULT_DISPATCH_TIMEOUT` matches the FSM's existing per-stage
budget (`stage_timeout` → 60s default, override via
`NEURON_SWARM_STAGE_TIMEOUT_SEC`). Reuses the existing
`commands::swarm::stage_timeout()` helper so the env knob stays
single-source.

### 2. Registry integration `src-tauri/src/swarm/agent_registry.rs`

Add a parallel dispatcher map keyed on `(workspace_id, agent_id)`:

```rust
pub struct SwarmAgentRegistry {
    // ... existing fields ...
    /// W5-02 — per-(workspace, agent) dispatcher tasks. Lazy-spawn
    /// on first `MailboxEvent::TaskDispatch` whose target matches.
    dispatchers: RwLock<HashMap<(String, String), MailboxAgentDispatcher>>,
}
```

New method:

```rust
/// Ensure a dispatcher exists for the given (workspace, agent).
/// Idempotent — second call is a no-op. Called by W5-03's brain
/// before emitting the first dispatch, OR lazily by a workspace-
/// level "ensure all dispatchers" sweep on first JobStarted.
pub async fn ensure_dispatcher<R: Runtime>(
    self: &Arc<Self>,
    app: &AppHandle<R>,
    workspace_id: &str,
    agent_id: &str,
    bus: &Arc<MailboxBus>,
);
```

Workspace shutdown path (`SwarmAgentRegistry::shutdown_all` —
existing W4-02 method): walk the dispatchers map; await
`shutdown()` on each before tearing down sessions. Order
matters — dispatchers must drain before sessions die so
in-flight invokes don't wake to a dead session.

### 3. New IPC `swarm:agents:dispatch_to_agent`

`src-tauri/src/commands/swarm.rs`:

```rust
/// W5-02 — manually dispatch one prompt to one agent via the
/// mailbox bus. Useful for tests + the orchestrator's debug
/// "manual dispatch" affordance. Production dispatch goes through
/// the W5-03 brain's loop.
///
/// Side effects: emits `MailboxEvent::TaskDispatch` to the
/// workspace's bus. The matching agent's dispatcher (lazy-
/// spawned if absent) consumes the event and runs the turn,
/// then emits `MailboxEvent::AgentResult` back.
///
/// Returns the dispatch row's `id` so callers can correlate the
/// later `AgentResult` (whose `parent_id == dispatch.id`).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_dispatch_to_agent<R: Runtime>(
    app: AppHandle<R>,
    bus: State<'_, Arc<MailboxBus>>,
    registry: State<'_, Arc<SwarmAgentRegistry>>,
    workspace_id: String,
    agent_id: String,
    prompt: String,
    job_id: Option<String>,
    with_help_loop: Option<bool>,
) -> Result<i64, AppError>;
```

`job_id` defaults to a fresh `j-<ULID>` if absent (so callers
that just want a one-off dispatch don't have to mint one).
`with_help_loop` defaults to `false` for W5-02 (help loop
migration is W5-03 territory).

### 4. Module re-exports

`src-tauri/src/swarm/mod.rs` re-exports `MailboxAgentDispatcher`
and `parse_agent_target`.

### 5. Bindings regen

`pnpm gen:bindings` produces:
- New command `commands.swarmAgentsDispatchToAgent`
- No new types (`MailboxEvent` already registered in W5-01)

Commit the regenerated `app/src/lib/bindings.ts`.

## Out of scope (per W5-overview)

- ❌ Help-loop migration to mailbox events (deferred to W5-03; the
  W4-05 transparent help loop inside
  `acquire_and_invoke_turn_with_help` keeps working unchanged).
  W5-02 dispatchers call the non-help variant only.
- ❌ Coordinator brain dispatch loop (W5-03).
- ❌ Job state derivation from the AgentResult chain (W5-04).
- ❌ Cancel migration end-to-end — W5-02 wires the
  `MailboxEvent::JobCancel` → `Notify::notify_one` step inside
  the dispatcher, but the IPC-level `swarm:cancel_job` still
  signals the FSM's per-job notify directly (W5-05 migrates it).
- ❌ Reviewer/Tester invoke (today via FSM stages directly; W5-02
  doesn't change this — the new IPC + dispatcher path is
  parallel).
- ❌ Multi-job-per-workspace (still serialised at the bus level
  — W5-05 enforces, W5-02 doesn't worry).

## Acceptance criteria

A sub-agent must self-verify each box before returning:

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` exits 0; total count ≥ **W5-01 baseline + 12**
      (12+ new unit tests as listed below)
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regenerates `app/src/lib/bindings.ts`;
      diff is checked into the commit
- [ ] `pnpm gen:bindings:check` exits 0 (post-commit)
- [ ] `pnpm typecheck` / `pnpm lint` / `pnpm test --run` exit 0
      (frontend test count unchanged; this WP has no frontend test
      additions)
- [ ] All tests listed below exist and pass:
  - [ ] `parse_agent_target_strips_prefix`
  - [ ] `parse_agent_target_rejects_missing_prefix`
  - [ ] `parse_agent_target_rejects_empty_id`
  - [ ] `dispatcher_routes_matching_target` — emit dispatch with
        target `agent:scout`; dispatcher for scout invokes; emits
        AgentResult with parent_id=dispatch.id
  - [ ] `dispatcher_ignores_non_matching_target` — emit dispatch
        targeted at `agent:planner` while only the scout
        dispatcher exists; no invoke fires
  - [ ] `dispatcher_emits_agent_result_with_parent_id` — assert
        the AgentResult row's parent_id matches the dispatch's id
  - [ ] `dispatcher_emits_error_result_on_invoke_failure` — mock
        registry returns Err; dispatcher still emits AgentResult
        with `assistant_text: "error: ..."`, `cost: 0`, `turn: 0`
  - [ ] `dispatcher_cancels_in_flight_invoke_on_job_cancel` —
        emit dispatch with 5s mock latency, then emit
        JobCancel{job_id}; assert dispatcher returns within 2s
  - [ ] `dispatcher_ignores_job_cancel_for_other_job` — mock
        slow invoke for job_id="j-1", emit JobCancel{"j-2"};
        invoke completes normally
  - [ ] `dispatcher_handles_lagged_receiver` — fill broadcast
        capacity (>64) + emit one more; dispatcher logs warn but
        keeps running, picks up later events
  - [ ] `dispatcher_shutdown_drains_cleanly` — spawn dispatcher,
        emit dispatch, call shutdown() while invoke runs,
        assert task exits within 5s
  - [ ] `swarm_agents_dispatch_to_agent_emits_dispatch_event` —
        IPC call with subscribed listener confirms a `task_dispatch`
        envelope arrives
  - [ ] `swarm_agents_dispatch_to_agent_validates_inputs` — empty
        workspace / agent_id / prompt rejected
  - [ ] `registry_ensure_dispatcher_is_idempotent` — calling
        `ensure_dispatcher` twice for the same (workspace, agent)
        produces only one dispatcher

## Verification commands

```powershell
cd src-tauri
cargo build --lib
cargo test --lib
cargo check --all-targets
cd ..

pnpm gen:bindings
git add app/src/lib/bindings.ts
pnpm gen:bindings:check
pnpm typecheck
pnpm lint
pnpm test --run
```

## Files allowed to modify

The sub-agent MAY create/edit:

- `src-tauri/src/swarm/agent_dispatcher.rs` (new)
- `src-tauri/src/swarm/agent_registry.rs` (add dispatchers map +
  `ensure_dispatcher`; update `shutdown_all` to drain
  dispatchers; do NOT remove existing `acquire_and_invoke_turn` /
  `_with_help` methods)
- `src-tauri/src/swarm/mod.rs` (re-export
  `MailboxAgentDispatcher`, `parse_agent_target`)
- `src-tauri/src/commands/swarm.rs` (add
  `swarm_agents_dispatch_to_agent`)
- `src-tauri/src/lib.rs` (specta + collect_commands wiring)
- `app/src/lib/bindings.ts` (regenerated only)
- `docs/work-packages/WP-W5-02-agent-mailbox-subscription.md`
  (status flip + Result section)
- `AGENT_LOG.md` (append entry)

The sub-agent MUST NOT touch:

- `src-tauri/src/swarm/coordinator/` (FSM stays; W5-06 deletes)
- `src-tauri/src/swarm/help_request.rs` (W4-05 substrate intact;
  W5-03 migrates to mailbox)
- `src-tauri/src/swarm/persistent_session.rs` /
  `src-tauri/src/swarm/transport.rs` (transport unchanged)
- `src-tauri/src/swarm/mailbox_bus.rs` (W5-01 surface frozen for
  this WP)
- Any persona file (`src-tauri/src/swarm/agents/*.md`)
- Any frontend component

## Notes / risks

- **Lazy vs eager dispatcher spawn**: the WP authors lazy spawn
  (first dispatch with matching target). Alternative: eager
  spawn on first `JobStarted` — wires all 9 dispatchers up
  front. Lazy is simpler and matches W4-02 session semantics
  (lazy spawn). If a future WP shows dispatch latency on the
  first invoke is a concern, switch to eager.
- **Cancel race**: between `current_invoke.lock()` reading the
  active (job_id, notify) and the dispatcher's main loop
  clearing the slot after invoke completes. The race is benign
  — `JobCancel` arriving microseconds after a successful invoke
  finishes is a no-op (notify on a dropped receiver fires
  harmlessly). Documented inline.
- **Lagged receiver recovery**: `RecvError::Lagged` skips events
  the dispatcher missed. The SQL log has them; W5-04's
  projector reads from SQL, not from the live broadcast, so UI
  state is unaffected. The agent itself just doesn't run those
  dispatches — which is OK because the brain (W5-03) won't emit
  another dispatch to the same agent until it sees the previous
  agent_result, so a lagged dispatcher self-recovers on the
  next emit cycle.
- **Help-loop deferred**: W5-02 dispatchers call
  `acquire_and_invoke_turn` (no help loop). If a specialist
  emits a `neuron_help` block via the W5-02 dispatch path, it
  surfaces as plain `assistant_text` inside the AgentResult —
  the brain (W5-03) parses it client-side and routes via the
  mailbox. The W4-05 substrate stays intact for FSM-driven
  invokes through `RegistryTransport`.
- **`AgentResult` always emitted**: even on invoke failure, the
  dispatcher emits an `AgentResult` event (with error text in
  `assistant_text`). This keeps the W5-04 projector's stream
  uniform — every dispatch produces exactly one result event.
  Without this, the brain would have to time-out waiting for a
  result that never comes.
- **Turn-cap respawn from W4-02 still applies**: the registry's
  `acquire_and_invoke_turn` enforces the turn cap; the
  dispatcher doesn't see this. After a respawn, the next invoke
  uses the fresh session. No new logic needed.

## Sub-agent prompt template

When the orchestrator dispatches a sub-agent for this WP, the
prompt should include:

1. The full text of this file (verbatim paste)
2. A pointer to `PROJECT_CHARTER.md` for scope conflicts
3. The "Verification commands" block as the self-verify gate
4. The "Files allowed to modify" list as the scope boundary
5. The "Acceptance criteria" checklist
6. AGENTS.md reminder: "no `--no-verify` commits; conventional
   commit message; co-author line per AGENTS.md §Commits"

## Result

(Filled in by the sub-agent on completion.)

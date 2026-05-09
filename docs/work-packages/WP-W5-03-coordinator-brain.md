---
id: WP-W5-03
title: Coordinator brain protocol + broadcast dispatch (mailbox-driven dispatch loop)
owner: TBD
status: not-started
depends-on: [WP-W5-01, WP-W5-02]
acceptance-gate: "New `CoordinatorBrain` service that subscribes to the workspace's `MailboxBus`, drives a dispatch loop on the Coordinator persona session, and emits `MailboxEvent::TaskDispatch` / `JobFinished` / `CoordinatorHelpOutcome` events based on parsed brain actions. New `swarm:run_job_v2` IPC parallel to `swarm:run_job` (the FSM-driven path stays). Updated `coordinator.md` persona with the dispatch-action JSON contract. Defense-in-depth `parse_brain_action` parser (4 strategies). Max-dispatches cap (default 30, env override). At least one real-claude integration smoke (`#[ignore]`d) drives a full job through the brain end-to-end. NO change to the existing FSM, `swarm:run_job` IPC, or W4-05 help-loop fallback. `cargo test --lib` ≥ 25 new unit tests; `pnpm typecheck` / `lint` / `gen:bindings:check` green."
---

## Goal

Replace the deterministic FSM stage iteration (Scout → Classify → Plan
→ Build×N → Review×N → Test) with a Coordinator-driven dispatch loop:
the Coordinator persona session reads the user goal + agent results
from the mailbox, decides what to dispatch next, and the bus
plumbing carries the dispatches to the right agents (W5-02
dispatchers).

This is the substantive WP of the W5 series. After W5-03 lands:

- `swarm:run_job_v2` is callable end-to-end against a real claude
  team and produces a Done / Failed outcome via mailbox events.
- The deterministic FSM is still alive (gated behind
  `swarm:run_job`) so behavior comparison + regression smokes
  stay possible until W5-06 deletes it.
- The W4-05 transparent help-loop is deprecated for v2 paths;
  W5-03 wires the mailbox-mediated alternative
  (`AgentHelpRequest` → brain parses → emits
  `CoordinatorHelpOutcome` → dispatcher feeds back to specialist).
- The 3×3 grid + Orchestrator chat panel (W3-12k3 / W4-04) keep
  working unchanged because they consume per-agent event channels
  (W4-03), not job-level events.

## Why now

Owner directive 2026-05-09 §1: relax the FSM into a fully
autonomous mailbox-driven swarm. W5-01 + W5-02 shipped the
substrate; W5-03 is the substrate's first real consumer and the
piece that makes the mailbox the *driver*, not just a side
channel.

## Charter alignment

- **Tech stack**: no new dependency. Reuses
  `tokio::sync::broadcast::Receiver`, `PersistentSession`,
  `tokio::sync::Notify`, the W4-02 registry, the W5-01 bus, and
  the W5-02 dispatchers.
- **Frontend mock shape**: the W5-04 projector preserves the
  existing `SwarmJobEvent` channel + `Job` / `JobOutcome` /
  `StageResult` shapes. W5-03 emits primitive mailbox events;
  the projector synthesises the legacy job-event stream from
  them. Frontend hooks unchanged.
- **Identifier strategy** (ADR-0007): the brain mints job_ids the
  same way the FSM does (`j-<ULID>` per ADR-0007 §2). No new
  identifier domain.
- **Timestamp invariant** (Charter §8): the brain emits events
  whose envelopes carry `ts` (epoch seconds, the existing column
  shape). No new timestamp fields in payloads.

## Scope

### 1. Persona update — `src-tauri/src/swarm/agents/coordinator.md`

The W3-12f Coordinator persona handled a single Classify decision
("research_only" vs "execute_plan"). W5-03 extends it to drive
the full dispatch loop. Add a new section to the persona body:

```markdown
## Dispatch protocol (W5-03)

You are the Coordinator. Your single job per turn is to decide the
NEXT dispatch — what should happen now, given the user goal and the
recent agent results.

Output exactly ONE fenced JSON block per turn. Pick one of:

```json
{"action": "dispatch", "target": "scout|planner|backend-builder|frontend-builder|backend-reviewer|frontend-reviewer|integration-tester", "prompt": "<verbatim user message you want sent>", "with_help_loop": true}
```

```json
{"action": "finish", "outcome": "done|failed", "summary": "<one-line wrap-up>"}
```

```json
{"action": "ask_user", "question": "<question to surface in the user chat>"}
```

```json
{"action": "help_outcome", "target": "<agent_id>", "body": {"action": "direct_answer", "answer": "..."}}
```

Constraints:
- The `prompt` for builders MUST include the relevant Plan output.
- Reviewers/Tester emit a JSON Verdict — read it and decide
  whether to re-dispatch the corresponding builder (with rejection
  feedback) or `finish`.
- Don't ask_user unless you genuinely cannot proceed without
  clarification. The user is your last resort, not your first.
- Pick `finish: failed` only after exhausting reasonable retries.
```

The Turkish-language persona body stays Turkish; the dispatch
contract section gets a Turkish wrapper too (mirrors the existing
pattern in `verdict.md` / `coordinator.md`).

### 2. New module `src-tauri/src/swarm/brain.rs`

```rust
//! `CoordinatorBrain` — mailbox-driven dispatch loop (WP-W5-03).
//!
//! One brain task per running job. Subscribes to the workspace
//! `MailboxBus`, holds a `PersistentSession` for the `coordinator`
//! agent, and drives the loop:
//!
//! 1. Render initial prompt from `JobStarted.goal`.
//! 2. Invoke the Coordinator session; parse the assistant_text
//!    via `parse_brain_action`.
//! 3. Match the action:
//!    - Dispatch → emit `TaskDispatch`
//!    - Finish → emit `JobFinished`, exit loop
//!    - AskUser → emit `JobFinished { outcome: "ask_user" }`,
//!      Orchestrator chat panel renders the question
//!    - HelpOutcome → emit `CoordinatorHelpOutcome`, continue loop
//! 4. Wait for next mailbox event (`AgentResult`,
//!    `AgentHelpRequest`, `JobCancel`); render next turn from it;
//!    loop to step 2.
//!
//! Termination guards:
//! - `max_dispatches` cap (default 30; env
//!   `NEURON_BRAIN_MAX_DISPATCHES`) — exceeding emits
//!   `JobFinished { outcome: "failed", summary: "exceeded max
//!   dispatches" }`.
//! - `JobCancel` mid-loop — emits `JobFinished { outcome: "failed",
//!   summary: "cancelled by user" }`.
//! - Coordinator session crash — emits `JobFinished { outcome:
//!   "failed", summary: "<error>" }`.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Runtime};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope, MailboxEvent};

/// One action the Coordinator persona can emit per turn.
/// Tagged on `action` (snake_case) to match the persona's JSON
/// contract verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BrainAction {
    /// Dispatch a turn to one specialist agent.
    Dispatch {
        target: String,
        prompt: String,
        #[serde(default)]
        with_help_loop: bool,
    },
    /// Job complete. `outcome` is "done" or "failed".
    Finish {
        outcome: String,
        summary: String,
    },
    /// Surface a question to the user via the Orchestrator chat
    /// panel. The brain pauses pending user response.
    AskUser {
        question: String,
    },
    /// Reply to an `AgentHelpRequest`. Body matches the
    /// `swarm::help_request::CoordinatorHelpOutcome` variants.
    HelpOutcome {
        target: String,
        body: serde_json::Value,
    },
}

/// Default brain dispatch cap. Tunable via
/// `NEURON_BRAIN_MAX_DISPATCHES`.
pub const DEFAULT_MAX_DISPATCHES: u32 = 30;

/// Defense-in-depth parser for `BrainAction`. Same 4-strategy
/// shape as W3-12d Verdict / W3-12f Decision / W4-05 HelpRequest:
/// 1. Whole-text JSON
/// 2. ```json fence strip
/// 3. First balanced `{...}` substring
/// 4. Bail with structured error
pub fn parse_brain_action(
    assistant_text: &str,
) -> Result<BrainAction, AppError>;

/// Spawn the brain task for one running job. Owns its
/// PersistentSession (acquired from registry); drops it on exit.
pub struct CoordinatorBrain;

impl CoordinatorBrain {
    /// Drive the dispatch loop end-to-end. Blocks until the brain
    /// emits `JobFinished` (success / failure / cancel).
    /// Returns the final outcome string ("done" / "failed" /
    /// "ask_user").
    pub async fn run<R: Runtime>(
        app: AppHandle<R>,
        workspace_id: String,
        job_id: String,
        goal: String,
        registry: Arc<SwarmAgentRegistry>,
        bus: Arc<MailboxBus>,
        cancel: Arc<Notify>,
    ) -> Result<String, AppError>;
}
```

### 3. New IPC `swarm:run_job_v2`

`src-tauri/src/commands/swarm.rs`:

```rust
/// W5-03 — mailbox-driven job runner. Mints a job_id, emits
/// `MailboxEvent::JobStarted`, spawns a `CoordinatorBrain` task,
/// awaits the brain's `JobFinished`, returns a `JobOutcome`
/// derived from the mailbox event log (W5-04 projector covers the
/// derivation; W5-03 calls the projector inline at the end).
///
/// Workspace lock semantics same as `swarm:run_job` — second
/// concurrent call for the same workspace returns `WorkspaceBusy`.
/// W5-05 migrates the lock to be derived from `JobStarted` events;
/// W5-03 reuses the existing `JobRegistry::try_acquire_workspace`
/// path for compatibility.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_run_job_v2<R: Runtime>(
    app: AppHandle<R>,
    pool: State<'_, DbPool>,
    bus: State<'_, Arc<MailboxBus>>,
    registry: State<'_, Arc<SwarmAgentRegistry>>,
    job_registry: State<'_, Arc<JobRegistry>>,
    workspace_id: String,
    goal: String,
) -> Result<JobOutcome, AppError>;
```

### 4. Help-loop migration through the bus

Today's W4-05 `acquire_and_invoke_turn_with_help` runs the help
loop transparently inside the registry — Coordinator session is
called directly. W5-03 introduces a parallel path:

- **W5-02 dispatchers** call the *non-help* variant
  (`acquire_and_invoke_turn`). The specialist's `assistant_text`
  is parsed for `neuron_help` blocks **inside the dispatcher**
  (not via the W4-05 transparent helper).
- On hit: emit `MailboxEvent::AgentHelpRequest` to the bus.
- The brain (W5-03) subscribes to AgentHelpRequest events; on
  receipt, renders a help-prompt into its Coordinator session;
  parses the response as `BrainAction::HelpOutcome`; emits
  `MailboxEvent::CoordinatorHelpOutcome`.
- The dispatcher subscribes to CoordinatorHelpOutcome events and
  feeds the body back to the specialist as a new turn.

Implementation:
- W5-02 dispatcher gets a new option: `with_help_loop: bool` field
  on `TaskDispatch`. When true, dispatcher does the help-block
  parsing + bus round-trip; when false, returns the raw result
  immediately.
- The existing W4-05 substrate stays for FSM-driven
  `swarm:run_job` (back-compat). v2 path uses the mailbox helper.

This adds parsing logic to `agent_dispatcher.rs` (W5-02 mod);
W5-03 expands the dispatcher's behavior to honor the
`with_help_loop` flag fully.

### 5. Module wiring

- `src-tauri/src/swarm/mod.rs`: re-export `BrainAction`,
  `parse_brain_action`, `CoordinatorBrain`,
  `DEFAULT_MAX_DISPATCHES`.
- `src-tauri/src/lib.rs`: register `BrainAction` as a specta type;
  add `swarm_run_job_v2` to `collect_commands!`.

### 6. Tests

≥ 25 new unit tests + ≥ 1 ignored real-claude integration smoke:

- **Parser** (≥ 8 tests):
  - parse_dispatch_action_basic
  - parse_dispatch_action_with_default_help_loop
  - parse_finish_action_done
  - parse_finish_action_failed
  - parse_ask_user_action
  - parse_help_outcome_action_direct_answer
  - parse_help_outcome_action_ask_back
  - parse_help_outcome_action_escalate
  - parse_handles_fenced_json_block
  - parse_handles_first_balanced_object
  - parse_rejects_unknown_action
  - parse_rejects_malformed_json

- **Brain run loop** (≥ 12 tests, mock registry + mock bus):
  - brain_emits_first_dispatch_after_job_started
  - brain_consumes_agent_result_emits_next_dispatch
  - brain_emits_finish_done_terminates_loop
  - brain_emits_finish_failed_terminates_loop
  - brain_emits_ask_user_terminates_with_ask_user_outcome
  - brain_consumes_help_request_emits_help_outcome
  - brain_max_dispatches_cap_terminates_with_failed
  - brain_cancel_mid_loop_terminates_with_failed
  - brain_handles_coordinator_session_crash
  - brain_resumes_loop_after_help_outcome
  - brain_emits_dispatch_with_correct_parent_id_chain
  - brain_finish_outcome_other_than_done_or_failed_normalised_to_failed

- **`swarm_run_job_v2`** (≥ 4 tests):
  - run_job_v2_validates_inputs
  - run_job_v2_workspace_busy_when_concurrent
  - run_job_v2_runs_full_chain_via_mock_brain
  - run_job_v2_returns_job_outcome_with_correct_shape

- **Real-claude integration smoke** (`#[ignore]`d):
  - `integration_run_job_v2_real_claude` — small goal, asserts
    `JobOutcome.final_state` is `Done`. Wall-clock budget 600s.

## Out of scope

- ❌ FSM teardown — W5-06 deletes the FSM. W5-03 ships v2 alongside.
- ❌ Job state derivation from mailbox + UI plumbing — W5-04. The
  v2 IPC returns a stub `JobOutcome` for now (built from the
  brain's emitted events; W5-04 makes this canonical).
- ❌ Cancel + workspace lock migration — W5-05. v2 uses the
  existing `JobRegistry::try_acquire_workspace` for compatibility.
- ❌ UI changes — `swarm:run_job_v2` is callable from the
  Orchestrator chat panel without panel changes (the panel
  already wires `useRunSwarmJob` against `swarm:run_job`; a
  followup polish swaps the call).
- ❌ Reviewer/Tester help-via-Verdict — still out per W4 overview;
  the brain reads JSON Verdict from Reviewer/Tester
  `AgentResult.assistant_text` directly.
- ❌ Multi-job concurrency in a single workspace — workspace lock
  stays.
- ❌ Brain memory beyond the persistent session — the
  Coordinator's stream-json context IS the memory.

## Acceptance criteria

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` exits 0; total count ≥ **W5-02 baseline + 25**
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regenerates `bindings.ts`; committed
- [ ] `pnpm gen:bindings:check` exits 0 post-commit
- [ ] `pnpm typecheck` / `pnpm lint` / `pnpm test --run` exit 0
- [ ] `integration_run_job_v2_real_claude` PASSES (or documented
      caveat with iteration log if LLM-flaky on first run)
- [ ] All listed unit tests exist + pass

## Verification commands

```powershell
cd src-tauri
cargo build --lib
cargo test --lib
cargo check --all-targets

# Real-claude smoke (manual, after toolchain + claude login)
$env:NEURON_BRAIN_MAX_DISPATCHES="15"
cargo test --lib integration_run_job_v2_real_claude -- --ignored --nocapture
cd ..

pnpm gen:bindings
git add app/src/lib/bindings.ts
pnpm gen:bindings:check
pnpm typecheck
pnpm lint
pnpm test --run
```

## Files allowed to modify

- `src-tauri/src/swarm/agents/coordinator.md` (persona update)
- `src-tauri/src/swarm/brain.rs` (new)
- `src-tauri/src/swarm/agent_dispatcher.rs` (extend W5-02
  dispatcher with `with_help_loop` parsing logic)
- `src-tauri/src/swarm/mod.rs` (re-exports)
- `src-tauri/src/swarm/profile.rs` (if persona frontmatter
  changes; usually not)
- `src-tauri/src/commands/swarm.rs` (add `swarm_run_job_v2`)
- `src-tauri/src/lib.rs` (specta + collect_commands)
- `app/src/lib/bindings.ts` (regen only)
- `docs/work-packages/WP-W5-03-coordinator-brain.md` (status flip
  + Result section)
- `AGENT_LOG.md` (entry)

MUST NOT touch:
- `src-tauri/src/swarm/coordinator/fsm.rs` (FSM stays for v1 path)
- `src-tauri/src/swarm/help_request.rs` (W4-05 substrate stays
  for the v1 transparent help loop; v2 has its own mailbox path)
- `src-tauri/src/swarm/persistent_session.rs` /
  `src-tauri/src/swarm/transport.rs`
- `src-tauri/src/swarm/mailbox_bus.rs` (W5-01 surface frozen)
- Any non-coordinator persona file
- Any frontend component (W5-04 owns UI plumbing)

## Split escape hatch

If the sub-agent kickoff hits a wall (test count blowing past
target, scope discovery on a tricky help-loop case), split:

- **W5-03a** — `BrainAction` enum + `parse_brain_action` parser
  + `CoordinatorBrain::run` happy path (Dispatch → AgentResult →
  Finish). Persona update. Mock-bus tests only. ~12 tests.
- **W5-03b** — Help-loop migration (`AgentHelpRequest` →
  `HelpOutcome` round-trip in the brain) + max-dispatches guard
  + cancel + real-claude smoke. ~13 tests + 1 integration.

The split point is the parser → the parser must work end-to-end
in 03a; 03b adds the loop's edge cases.

## Notes / risks

- **Brain context-bloat under long jobs**: every dispatch round
  adds one user-message + one assistant-message to the
  Coordinator session's stream-json context. A 30-dispatch job
  pushes ~60 messages. The W4-02 turn-cap (200 default) is well
  past this; respawn-with-replay would lose mid-job context but
  is rarely triggered in practice.
- **Brain LLM nondeterminism on dispatch order**: same goal might
  dispatch (scout, planner, builder) one run and (scout, builder,
  planner) on another. Mitigation: persona body's hard contract
  constraints ("Builder requires Plan output"). If LLMs ignore,
  W5-03b adds a post-parse validator that re-prompts on illegal
  dispatch.
- **AskUser flow incomplete**: W5-03 emits
  `JobFinished { outcome: "ask_user" }`. The Orchestrator chat
  panel does NOT yet listen for this — surfacing the question to
  the user is W5-04 / future polish. For now, the user sees a
  failed job with the question in `summary`; they re-run with
  the answer in the goal.
- **Verdict integration**: Reviewer/Tester `AgentResult` carries
  the Verdict JSON in `assistant_text`. The brain's persona body
  reads it and decides retry vs finish. No structured
  `Verdict` field on `AgentResult` — would couple the bus to the
  Verdict shape. Trade-off: brain has to parse JSON inside its
  prompt, which it's already doing for dispatch actions.
- **W5-02 dispatcher with_help_loop scope**: W5-02 ships the flag
  but treats it as a no-op (W5-02's `acquire_and_invoke_turn`
  has no help loop). W5-03 makes the flag functional inside the
  dispatcher (parses for `neuron_help` blocks, emits to bus,
  awaits CoordinatorHelpOutcome). This is the only place W5-03
  edits W5-02's `agent_dispatcher.rs`.

## Result

(Filled in by the sub-agent on completion.)

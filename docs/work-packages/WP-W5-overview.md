---
id: WP-W5-overview
title: Week 5 — Mailbox-driven autonomous swarm (FSM → message-bus)
owner: orchestrator
status: planning
---

# Week 5 — Master plan

This document is the planning companion to the per-WP files
(`WP-W5-01-*` … `WP-W5-06-*`). It captures the scope, dependency
graph, and rationale that the individual WPs reference. Per
`AGENTS.md`, each per-WP file is the contract a sub-agent works
against; this file is **not** a contract — it is the orchestrator's
map of how the W3-shipped + W4-extended deterministic FSM is being
relaxed into a fully autonomous mailbox-driven swarm.

## Source of scope

Every Week 5 line item is tracked back to one of:

- **Owner directive 2026-05-09** — the W4-overview decision 4B
  ("fully autonomous mailbox-driven swarm") was explicitly deferred
  to "a future WP-W5" by the W4 author. On 2026-05-09 the owner
  re-opened it as the next direction:
  > "Owner directive kısmında dediğim şekilde projeyi ilerletmeye
  > devam edelim" → resolved to W5: FSM → message-bus.

  The original 2026-05-07 directive ("her ajan kendi terminal'inde
  tek başına çalışsın, birbirleriyle iletişim de kurabilsin") still
  governs. W4 satisfied the *visibility* half via the 3×3 grid +
  per-agent event channel. W4-05's `neuron_help` substrate
  satisfied the *Coordinator-mediated inter-agent comms* half on a
  per-turn basis. W5 generalises that substrate from "rescue lane
  inside a stage" to "the actual dispatch loop", removing the
  hardcoded stage iteration in favour of Coordinator-broadcast
  dispatch through the mailbox.

- **WP-W4-overview.md** — decision 4B verbatim:
  > "The 'fully autonomous mailbox-driven swarm' alternative
  > (decision 4B) is rejected for W4. It's a strictly larger
  > refactor and the deterministic FSM has shipped value (see W3
  > acceptance gates). If after W4 the team wants to relax the FSM
  > into a full message-bus, that's a future WP-W5."

  W4 closed 2026-05-07 with the FSM still in place + persistent
  sessions + help-loop. W5 picks up the deferred refactor.

- **`report/Neuron Multi-Agent Orchestration` architectural report**
  — the original BridgeSwarm-shape vision specifies mailbox-mediated
  comms with each agent subscribing to its own queue. W3 deferred
  this to ship a working FSM-driven substrate first; W4 made the
  substrate visible + persistent. W5 is the deferred autonomy half.

- **`AGENT_LOG.md`** — the 2026-05-07 W4 closure entry that marks
  the persistent visible swarm runtime as PRODUCTION-READY at the
  registry / help-loop / grid level. W5 inherits a green W4.

If a Week 5 item appears here without one of those sources, it is
a scope addition and must be approved by the owner before the
matching WP file is authored.

## Status

| ID | Title | Owner | Status | Blocked by | Size |
|---|---|---|---|---|---|
| WP-W5-01 | Mailbox event-bus substrate (kind / parent_id / payload_json columns + workspace broadcast channel) | orchestrator-direct | **implemented 2026-05-09 (`1b92c63`); cargo/pnpm verification DEFERRED to user dev shell** | — | M |
| WP-W5-02 | Agent mailbox subscription + auto-emit (`MailboxAgentDispatcher` per agent) | sub-agent (general-purpose) | **shipped 2026-05-10 (`8cca3ba` + `2432440` + `14a50b3` + `739e836`); cargo 465/0/14 verified** | WP-W5-01 ✅ | M |
| WP-W5-03 | Coordinator brain protocol — broadcast dispatch (`CoordinatorBrain` + `BrainAction` parser + `swarm:run_job_v2`) | TBD | contract authored (`42a247d`); not started | WP-W5-02 | L |
| WP-W5-04 | Job state derived from mailbox + UI plumbing (`JobProjector` synthesises `SwarmJobEvent` stream) | TBD | contract authored (`42a247d`); not started | WP-W5-03 | M |
| WP-W5-05 | Cancel + workspace serialization under message-bus (`JobCancel` event + `JobStarted` workspace-busy guard) | TBD | contract authored (`42a247d`); not started | WP-W5-03 | S |
| WP-W5-06 | FSM deprecation + 435-test migration + final integration smoke (replace `swarm:run_job` with the brain dispatcher; remove `coordinator::fsm`) | TBD | contract authored (`42a247d`); not started | WP-W5-04, WP-W5-05 | L |

Sizes (rough, in sub-agent days): S = 0.5–1 day, M = 1–2 days,
L = 3+ days. W5-03 and W5-06 are L; both are split candidates if
their first sub-agent kickoff hits a wall — see §"Split escape
hatches" below.

## Dependency graph

```
WP-W5-01 (mailbox event-bus substrate)
   │
   └──► WP-W5-02 (agent mailbox subscription + auto-emit)
           │
           └──► WP-W5-03 (Coordinator broadcast-dispatch protocol)
                   │
                   ├──► WP-W5-04 (job state derived from mailbox + UI plumbing)
                   │       │
                   │       ▼
                   ├──► WP-W5-05 (cancel + workspace lock under message-bus)
                   │       │
                   │       ▼
                   └─────► WP-W5-06 (FSM deprecation + test migration + final smoke)
```

W5-04 and W5-05 are parallelizable after W5-03 lands. W5-06 absorbs
the integration cost of all upstream WPs and is the only WP that
deletes existing FSM code.

## Per-WP scope rationale

### WP-W5-01 — Mailbox event-bus substrate

The W2-02 `mailbox` table is already a generic event log
(`id`, `ts`, `from_pane`, `to_pane`, `type`, `summary`). W4-07
added swarm-aware namespacing: `agent:<id>` prefix on
from_pane/to_pane and `swarm.<verb>` prefix on type. The substrate
is *almost* what we need for a message-bus — but two gaps prevent
direct reuse for autonomous dispatch:

1. **Routing**: `to_pane` carries one target string. A broadcast
   ("any builder pick this up") needs a wildcard target. A reply-to
   chain (Coordinator dispatches → specialist runs → specialist
   emits result with `parent_id=dispatch.id`) needs a parent
   reference for correlation.
2. **Liveness**: there's no in-process broadcast. Today's mailbox
   listeners poll via `mailbox:list(sinceTs)`. Autonomous agents
   need wake-on-message latency, not 2s polling latency.

Scope:
- Migration `0010_mailbox_eventbus.sql`: add `kind TEXT NOT NULL
  DEFAULT 'note'`, `parent_id INTEGER`, `payload_json TEXT NOT NULL
  DEFAULT '{}'` columns. `kind` is the structured discriminator
  (`task.dispatch` / `agent.result` / `agent.help_request` /
  `coordinator.help_outcome` / `job.started` / `job.finished` /
  `job.cancel` / `note` for legacy). `payload_json` carries the
  typed body without the parser hop through `summary`. Existing
  rows backfill `kind='note'`, `payload_json='{}'`.
- Backend pubsub: workspace-scoped `tokio::sync::broadcast::Sender<
  MailboxEvent>` keyed on `workspace_id`. Held in a `MailboxBus`
  service that lives in `app.manage(...)` next to `SwarmAgentRegistry`.
- New IPC: `mailbox:emit_typed(workspace_id, kind, target, parent_id?,
  payload_json)` returning the inserted row. Does both: SQL
  insert + broadcast to the workspace's channel + Tauri
  `mailbox:new` event (back-compat).
- Specta-typed `MailboxEvent` enum with the variants listed above
  (no behavior wiring in this WP; just types + parser).
- `mailbox:list_typed(workspace_id, kind?, since_id?)` — typed
  filtered list. Replaces no existing IPC; existing
  `mailbox:list` stays for the terminal-pane use case.
- Tests:
  - migration round-trip (forward + backward)
  - `MailboxBus::subscribe(workspace_id)` receives broadcast emits
  - `emit_typed` persists + broadcasts + emits Tauri event
  - parser unit tests for each `MailboxEvent` variant (8+ shape
    variants including malformed payload_json)

**Source**: Owner directive 2026-05-09 §1 + W4-07 mailbox precedent.

**Out of scope** (deferred to W5-02+):
- Agent-side subscription wiring (W5-02)
- Coordinator dispatch protocol (W5-03)
- FSM removal (W5-06)

### WP-W5-02 — Agent mailbox subscription + auto-emit

W4's `SwarmAgentRegistry` exposes `acquire_and_invoke_turn`. W5-02
adds a parallel autonomous pull path: a per-(workspace, agent)
`MailboxAgentDispatcher` task that subscribes to the workspace
channel from W5-01 and feeds incoming `task.dispatch` events with
`target=agent:<this_id>` into the agent's session as new turns. The
agent's `result` is automatically emitted back as an `agent.result`
event with `parent_id` pointing at the dispatch.

This decouples invocation from the caller. Today the FSM (or test
harness, or `swarm:test_invoke` IPC) is always the explicit caller
of `acquire_and_invoke_turn`. After W5-02, agents are reachable
*by emitting a mailbox dispatch event* — exactly the loop W5-03's
Coordinator will drive.

Scope:
- New `MailboxAgentDispatcher` per (workspace, agent) pair. Spawned
  by the registry on first dispatch event; lives until workspace
  shutdown (mirrors session lifecycle).
- Dispatcher loop: `recv()` on the workspace channel → match
  variant on `MailboxEvent::TaskDispatch` → if `target ==
  format!("agent:{agent_id}")`, call `acquire_and_invoke_turn`
  (or `_with_help` based on a per-dispatch flag) → on result, emit
  `MailboxEvent::AgentResult { parent_id: dispatch.id,
  assistant_text, total_cost_usd, turn_count }` via
  `MailboxBus::emit_typed`.
- Cancel: dispatcher subscribes to `MailboxEvent::JobCancel
  { job_id }` and signals the inner `Notify` shared with
  `acquire_and_invoke_turn`'s cancel parameter.
- Specialist help-loop preserved: `_with_help` branches on
  `MailboxEvent::AgentHelpRequest` — emits the request as a mailbox
  event (instead of W4-05's direct registry call). Coordinator
  brain (W5-03) reads the help request from its own mailbox
  subscription and emits `coordinator.help_outcome` back.
- New `swarm:agents:dispatch_to_agent(workspace_id, agent_id,
  prompt, parent_id?)` IPC for tests / single-shot manual dispatch
  from the UI (gated behind a debug flag).
- Tests:
  - dispatch event with matching target → session invoke fires
  - dispatch event with non-matching target → no-op
  - agent.result emitted with correct parent_id
  - cancel via `JobCancel` propagates to in-flight invoke
  - help-request emit + outcome consume round-trip
  - dispatcher shutdown clean on workspace shutdown

**Source**: Owner directive 2026-05-09 §1 + W4-02 registry contract.

**Out of scope**:
- Coordinator brain logic (W5-03)
- Job state derivation (W5-04)
- FSM teardown (W5-06)

### WP-W5-03 — Coordinator brain protocol + broadcast dispatch

The substantive piece. The deterministic FSM (`Init → Scout →
Classify → Plan → Build → Review → Test → Done`) is replaced by a
Coordinator-driven dispatch loop:

1. User message lands at Orchestrator (W3-12k1) as today; on
   `Dispatch` action the Orchestrator emits `MailboxEvent::JobStarted
   { job_id, goal }` to the workspace channel.
2. Coordinator session (lazy-spawned per W4-02) is subscribed to
   the channel via a new `CoordinatorBrain` task. It receives
   `JobStarted` and renders the first turn: "User goal: {goal}.
   Available agents: scout / planner / backend-builder / ...
   What should run first?"
3. Coordinator persona emits a structured action — one of:
   - `{"dispatch": {"target": "scout", "prompt": "Investigate ..."}}`
   - `{"finish": {"outcome": "done", "summary": "..."}}` (job complete)
   - `{"finish": {"outcome": "failed", "summary": "..."}}` (job failed)
   - `{"ask_user": {"question": "..."}}` (escalate to Orchestrator chat)
4. CoordinatorBrain parses → emits `MailboxEvent::TaskDispatch`
   (which W5-02's dispatcher picks up + routes to the named agent).
5. Specialist runs the turn → emits `MailboxEvent::AgentResult`
   (auto-emitted by W5-02).
6. CoordinatorBrain receives the AgentResult → renders the next
   turn into the Coordinator session: "scout returned: {summary}.
   What now?" → loop back to step 3.
7. Loop terminates when Coordinator emits `finish` → CoordinatorBrain
   emits `MailboxEvent::JobFinished { job_id, outcome }`.

Scope:
- New `CoordinatorBrain` service (`src-tauri/src/swarm/brain.rs`).
  One task per workspace; spawned by `swarm:run_job_v2` IPC.
- New `coordinator.md` persona refactor (or bundled-overlay) to
  include the dispatch-action JSON contract. The W3-12f
  `CoordinatorDecision` parser shape is reused for Classify-only
  decisions; the new dispatch parser is separate (`parse_brain_action`).
- Defense-in-depth parser (4-step like Verdict / Decision /
  HelpRequest):
  1. Whole-text JSON
  2. ```json``` fence strip
  3. First balanced `{...}` substring
  4. Bail → emit `MailboxEvent::JobFinished { outcome: "failed",
     summary: "Coordinator output unparseable" }` (fail-loud, no
     retry — the Coordinator brain bug is rare and shouldn't ride
     a silent retry loop)
- New `swarm:run_job_v2` IPC parallel to `swarm:run_job` (FSM stays
  for back-compat through W5-05; W5-06 deletes it). Same input
  shape (`workspace_id`, `goal`); same workspace lock semantics.
  Returns `JobOutcome` derived from mailbox events (W5-04 covers
  the derivation).
- Verdict + Reviewer integration: Reviewer/Tester are still invoked
  via dispatch, but they emit `MailboxEvent::AgentResult` whose
  `payload_json` carries a parsed Verdict. CoordinatorBrain reads
  the Verdict and decides retry/done — *the retry loop is now part
  of the Coordinator brain, not a hardcoded `'retry_loop`*. This is
  the entire reason for the W5 refactor: dynamic decisions, not
  hardcoded pipelines.
- Termination guard: CoordinatorBrain enforces a hard cap on total
  dispatches per job (default 30, env override
  `NEURON_BRAIN_MAX_DISPATCHES`) so a runaway brain can't spin
  forever. Hitting the cap emits `JobFinished { outcome: "failed",
  summary: "exceeded max dispatches" }`.
- Tests:
  - happy-path mock dispatch chain (scout → planner → builder →
    reviewer → finish)
  - rejected-verdict re-dispatch (reviewer rejects → coordinator
    re-dispatches builder with feedback)
  - parser tests for each action variant (10+ shape variants)
  - max-dispatches cap fires correctly
  - cancel mid-dispatch propagates
  - real-claude integration smoke (`#[ignore]`d): full job
    end-to-end through the brain

**Source**: Owner directive 2026-05-09 §1 + W3-12f Coordinator
brain precedent + W4-overview decision 4B.

**Out of scope**:
- UI surface migration (W5-04)
- FSM teardown (W5-06)

**Risk note**: The Coordinator brain may produce sub-optimal
dispatch sequences relative to the hardcoded FSM order (which has
seven months of LLM-tuning behind it). Mitigation: the FSM stays
through W5-05 so a regression-comparison smoke can pin behavior;
W5-06 is gated on the v2 path matching v1's success rate on a
3-job battery (research-only / single-domain / fullstack).

### WP-W5-04 — Job state derived from mailbox + UI plumbing

The frontend's `useSwarmJob`, `useSwarmJobs`, and the `SwarmJobEvent`
channel (`swarm:job:{id}:event`) are pinned to today's FSM event
shape. W5-04 preserves the wire shape but sources payloads from
the mailbox subscription instead of FSM transitions:

- `Started` ← `MailboxEvent::JobStarted`
- `StageStarted` ← `MailboxEvent::TaskDispatch`
- `StageCompleted` ← `MailboxEvent::AgentResult`
- `Cancelled` ← `MailboxEvent::JobCancel` (intermediate)
- `Finished` ← `MailboxEvent::JobFinished`
- `RetryAttempt` ← derived from CoordinatorBrain dispatching the
  same builder twice with the same parent_id chain (compute
  `attempt = count(dispatches with same target since last
  finish-or-job-start)`)

Scope:
- New `JobProjector` service that subscribes to the workspace
  channel and synthesises `SwarmJobEvent`s. Emits onto the same
  `swarm:job:{id}:event` Tauri channel the FSM uses today. Frontend
  hooks unchanged.
- New `swarm_jobs_v2` table (or extend existing `swarm_jobs` with
  a `source: TEXT` column where `'fsm' | 'brain'`) to persist
  brain-driven jobs. Stage rows synthesised from
  `MailboxEvent::AgentResult` carry the same shape as today's
  `swarm_stages`.
- `JobOutcome` builder: walks the mailbox event log for a job and
  computes `final_state`, `total_cost_usd`, `total_duration_ms`,
  `last_error`, `last_verdict`. Used by `swarm:get_job` /
  `swarm:list_jobs` to return brain-driven jobs in the same wire
  shape as FSM jobs.
- `SwarmJobEvent::RetryAttempt` derivation: a `TaskDispatch` event
  whose `target` equals the most recent `AgentResult`'s `target`
  AND whose `parent_id` chain traces back to a Verdict-rejected
  result counts as a retry. The synthesiser increments a per-job
  retry counter and emits `RetryAttempt`.
- Tests:
  - projector emits Started → StageStarted → StageCompleted →
    Finished on a synthetic mailbox stream
  - retry detection fires correctly
  - JobOutcome computed from event log matches FSM-emitted shape
    (struct-equality assert on a fixture)
  - `swarm:get_job` returns brain-driven job in identical shape
    to FSM job

**Source**: Owner directive 2026-05-09 §1 (preserve UI
investment) + W3-14 SwarmJobEvent contract.

**Out of scope**:
- Removing FSM (W5-06)
- New UI features (none in W5; the existing 3×3 grid + chat
  panel + recent-jobs list cover W5)

### WP-W5-05 — Cancel + workspace serialization under message-bus

The W3-12c cancel path uses an in-process `tokio::sync::Notify`
shared between FSM and stage tasks. The W3-12a workspace lock is
a HashMap entry held for the whole FSM run via a Drop guard.
Both need re-implementation under the message-bus:

- **Cancel**: `swarm:cancel_job(job_id)` IPC emits
  `MailboxEvent::JobCancel { job_id }` to the workspace channel.
  `MailboxAgentDispatcher` subscribes (W5-02) and signals the inner
  Notify shared with `acquire_and_invoke_turn`'s cancel parameter,
  truncating the in-flight turn. CoordinatorBrain also subscribes
  and exits its dispatch loop, emitting `JobFinished { outcome:
  "failed", summary: "cancelled by user" }`.
- **Workspace lock**: still a per-workspace mutex (one job at a
  time per workspace stays the rule per Charter §9). But acquire
  is via emitting `JobStarted` to the channel — the
  `MailboxBus::emit_typed` path checks "is there an in-flight
  job for this workspace?" by querying the projector's state
  before broadcasting. If yes → reject with `WorkspaceBusy`. If no
  → emit + register the job_id as "in-flight" → release on
  `JobFinished`.

Scope:
- `MailboxBus::emit_typed` gains workspace-busy guard for
  `JobStarted` events. Returns `Err(AppError::WorkspaceBusy)` if
  another job is in-flight for the same workspace.
- `JobProjector` (W5-04) tracks in-flight jobs per workspace.
- `swarm:cancel_job` IPC migrated: emits `JobCancel` instead of
  signaling Notify directly. FSM's `signal_cancel` stays for
  back-compat through W5-06.
- Cancel-on-workspace-shutdown: `RunEvent::ExitRequested`
  (`lib.rs`) emits `JobCancel` for every in-flight job before
  calling `shutdown_all`. Matches today's eager-kill semantics.
- Tests:
  - cancel mid-dispatch propagates to all subscribed agents
  - workspace-busy guard rejects concurrent JobStarted
  - workspace shutdown cancels in-flight jobs cleanly
  - end-to-end cancel smoke (mock dispatcher + brain)

**Source**: Owner directive 2026-05-09 §1 + W3-12a workspace lock
contract + W3-12c cancel contract.

### WP-W5-06 — FSM deprecation + 435-test migration + final integration smoke

The teardown WP. Removes the old FSM and migrates its test suite.

Scope:
- **Migration plan for the 435 existing Rust tests** (cargo test
  --lib). Categorised:
  - **Pure FSM-internals tests** (e.g. `select_chain_pairs`,
    `aggregate_rejections`, `next_state_from`) — DELETED. The
    behavior they pin is gone; the brain's dispatch logic is
    LLM-driven and tested via end-to-end mock streams.
  - **Job lifecycle tests** (e.g. `fsm_happy_path_emits_finished`)
    — REWRITTEN against the brain dispatcher. Same job-level
    assertions, different driver.
  - **Persistence tests** (e.g. `job_persists_on_state_transition`)
    — KEPT, source switched to brain-emitted projections.
  - **Verdict / Decision / Help parsers** — KEPT verbatim. The
    parsers don't care about the dispatcher.
  - **Integration tests** (`integration_*_real_claude`) —
    REWRITTEN against `swarm:run_job_v2`. Same goal text, same
    timeout, asserts JobOutcome.final_state and stage count.
  - Estimated breakdown: ~80 tests deleted (pure FSM), ~120
    rewritten (lifecycle + integration), ~235 kept.
- **`coordinator::fsm` module deletion**: 7322 lines of
  `src-tauri/src/swarm/coordinator/fsm.rs` deleted. The FSM-side
  prompt templates (SCOUT_PROMPT_TEMPLATE / PLAN_PROMPT_TEMPLATE
  / BUILD_PROMPT_TEMPLATE / etc.) move to a new
  `src-tauri/src/swarm/prompts.rs` module and are referenced by
  the brain's dispatch instructions (or the persona files
  themselves; design decision deferred to W5-06 authoring time).
- **`swarm:run_job` IPC migration**: redirected to v2 internally;
  v2 IPC name stays for the test rewrite phase, then renamed back
  to `swarm:run_job` in a final commit. `bindings.ts` regen.
- **`CoordinatorFsm` import sites**: `commands/swarm.rs`,
  `lib.rs`, all test files. Each rewritten to use
  `CoordinatorBrain` instead.
- **Final real-claude integration battery**: the W3-12 smoke suite
  (research-only / single-domain backend / single-domain frontend
  / fullstack) re-run on the brain dispatcher. Each must reach the
  same final state (Done / Failed) as the FSM run on the same goal.
  Wall-time may differ (brain may take more dispatches than FSM's
  hardcoded chain) but final outcome must match. Documented as a
  W5 acceptance gate.
- **`AGENT_LOG.md` retrospective entry** capturing test-count
  delta, deleted-LOC count, brain-dispatcher avg dispatch count
  per goal vs FSM stage count.

**Source**: Owner directive 2026-05-09 §1 (full FSM removal) +
all upstream W5 WPs.

**Out of scope**:
- Multi-job-per-workspace (still serialised per Charter §9)
- Multi-workspace UX (still one workspace per app install per W4
  out-of-scope)
- Specialist-to-specialist direct comms without going through
  Coordinator (still Coordinator-mediated per W4-overview decision
  3C; the brain IS the Coordinator hub)

## Authoring sequence

The orchestrator authors per-WP files (`WP-W5-NN-*.md`) on demand,
not all up-front. Each WP file is written immediately before its
sub-agent kickoff, with the latest state of the codebase in
context. This document is the "what's next" reference — it is
allowed to drift slightly from per-WP files as scope is
discovered, but never silently: the diff is logged in
`AGENT_LOG.md` under "scope amendment".

Recommended sequence:

1. **W5-01** (event-bus substrate) — pure backend, schema +
   broadcast wiring + types. No behavior change. Lands first
   so the rest can build on it.
2. **W5-02** (agent subscription) — depends on W5-01; adds the
   pull-side dispatcher per agent. New `dispatch_to_agent` IPC
   for ad-hoc dispatch from tests.
3. **W5-03** (Coordinator brain) — depends on W5-02; the
   substantive piece. New `swarm:run_job_v2` IPC parallel to
   `swarm:run_job`. FSM untouched.
4. **W5-04** (job-state derivation) — depends on W5-03;
   preserves frontend wire shape. UI hooks unchanged.
5. **W5-05** (cancel + workspace lock) — depends on W5-03;
   parallelizable with W5-04 if a second sub-agent is available.
6. **W5-06** (FSM deprecation + test migration) — depends on
   W5-04 and W5-05; the destructive WP. Only WP that deletes
   shipped code.

Parallelisation: W5-04 and W5-05 can run in parallel after W5-03
lands.

## Split escape hatches

W5-03 and W5-06 are L-sized. If a sub-agent kickoff for either
hits a wall (test count blowing past target, scope discovery on
file X), the orchestrator splits before re-dispatching:

- **W5-03 split**: into `W5-03a` (CoordinatorBrain task structure
  + parser + happy-path) and `W5-03b` (verdict/retry integration
  + max-dispatches guard + real-claude smoke). The split point is
  the `parse_brain_action` parser → everything before stays in 03a,
  everything after lands in 03b.
- **W5-06 split**: into `W5-06a` (test migration; FSM stays alive)
  and `W5-06b` (FSM deletion). Allows the v2 path to soak in
  parallel with v1 for one cycle before the deletion ships.

The orchestrator decides on the split before authoring the per-WP
contract; once authored, scope is frozen for that sub-agent. Split
authoring is logged in `AGENT_LOG.md`.

## Out of scope (Week 5)

Lifted from the owner directive + Charter; restated here so per-WP
authors don't re-litigate:

- ❌ **Specialist-to-specialist direct comms** — by owner directive
  (W4 decision 3C, unchanged in W5), all inter-agent traffic goes
  through the Coordinator. The brain IS the Coordinator hub. A
  specialist emitting a `task.dispatch` event with `target =
  agent:other-specialist` is rejected by the brain's permission
  guard (only the brain can emit dispatches; specialists emit
  `agent.result` and `agent.help_request` only).
- ❌ **Custom user-defined dispatch policies** — the brain decides
  every dispatch. A "force this stage" or "skip this stage" UI
  affordance is post-W5. The owner directive resolves this:
  autonomous dispatch is the goal; manual override defeats the
  purpose.
- ❌ **Multi-workspace concurrency** — still one workspace per app
  install. The mailbox bus is keyed on `workspace_id` so the
  substrate is multi-workspace-ready, but the UI + lifecycle stay
  single-workspace.
- ❌ **Cross-app-restart job persistence** — sessions still die on
  app close. The mailbox event log persists (SQLite); recovery
  semantics for an in-flight job at restart are: `JobProjector`
  observes a `JobStarted` without a matching `JobFinished` →
  emits `JobFinished { outcome: "failed", summary: "interrupted
  by app restart" }` synthetically. Identical to today's
  `recover_orphans` sweep, just sourced from mailbox.
- ❌ **Streaming Coordinator decisions** — brain returns one
  dispatch action per turn (one-shot per turn, like W3-12k1's
  Orchestrator decide). Streaming is post-W5.
- ❌ **Brain memory beyond the persistent session** — the
  Coordinator session's stream-json context IS the memory. No
  separate vector store, no separate summarisation step. Same
  decision as W4-02's persistent session lifecycle.
- ❌ **Manual specialist invocation** — `swarm:agents:dispatch_to_agent`
  IPC (W5-02 scope) is debug-only. Production dispatch goes
  through the brain.

## Owner decisions (resolved)

Recorded here so per-WP authors don't re-litigate. Date noted on
each because Week 5 scope is allowed to evolve as we learn —
re-opening any of these is a scope amendment that lands in
`AGENT_LOG.md`.

1. **FSM disposition** (resolved 2026-05-09):
   The W3 FSM is removed at end of W5 (via W5-06). It survives
   through W5-05 to enable behavior-comparison smokes. The new
   driver is the Coordinator brain dispatching through the mailbox
   event-bus.

   Trade-off acknowledged: 7322 lines of well-tested FSM code
   gets deleted. ~80 unit tests die with it. Mitigation: the brain
   is reachable by the same `swarm:run_job` IPC + same
   `SwarmJobEvent` channel, so the frontend (3×3 grid + chat panel
   + recent-jobs list) is unchanged.

2. **Brain shape** (resolved 2026-05-09):
   One `coordinator.md` persona (extended from W3-12f's brain) emits
   structured `dispatch / finish / ask_user` actions. CoordinatorBrain
   is a backend Rust service that drives the dispatch loop based on
   parsed actions. Not a separate persona; not a separate session.

3. **Retry loop relocation** (resolved 2026-05-09):
   Verdict-gated retry moves from FSM's hardcoded `'retry_loop` to
   the brain's dispatch decisions. The brain reads a Reviewer's
   rejection (Verdict.approved=false) and decides whether to
   re-dispatch the builder, dispatch a different specialist, or
   give up. `MAX_RETRIES = 2` constant moves to a brain-side hint
   in the persona body, not a hardcoded loop.

4. **Workspace-busy enforcement** (resolved 2026-05-09):
   Stays at the bus emit layer (W5-05). `JobStarted` for a
   workspace with an in-flight job rejects with `WorkspaceBusy`.
   Same wire shape as today.

5. **Verdict/Decision parsers** (resolved 2026-05-09):
   Kept verbatim. The W3-12d Verdict parser is reused; Reviewer/
   Tester emit `agent.result` events whose `payload_json` carries
   the parsed Verdict. The brain reads it from the projection.

## Open questions (still gating)

None gating per-WP authoring as of 2026-05-09. Open questions
that may surface during W5-03 (brain) authoring:

- **Brain context-bloat under long jobs**: the Coordinator session's
  context grows by one user-message + one assistant-message per
  dispatch round. A 30-dispatch job could push past the
  context-window comfortable zone. Mitigation if observed: the
  `NEURON_BRAIN_MAX_DISPATCHES` cap (default 30) plus W4-02's
  turn-cap respawn (default 200) bound the growth. Mid-job
  summarisation is a future polish.
- **Brain LLM nondeterminism on dispatch order**: same reasonable
  goal might dispatch (scout, planner, builder) on one run and
  (scout, builder, planner) on another. If the second order
  fails persona contracts (planner expects scout output but
  builder ran in between), the brain has to recover. Mitigation:
  the brain persona body includes hard contract constraints
  ("Builder requires Plan output") that the LLM should respect.
  If LLMs ignore these, W5-03b adds a post-parse validator that
  rejects illegal dispatches and re-prompts the brain.

These get resolved at W5-03 authoring time, not now.

## Relationship to W3 + W4 backlog

The W3 backlog (W3-04 LangGraph cancel, W3-05 approval UI, W3-08
multi-workflow editor, W3-09 capabilities + E2E, W3-10 Python
embed) and the W4 follow-up list (Reviewer/Tester help-via-Verdict,
Swarm comms tab, per-event SQLite persistence, cross-restart
session persistence) are **not blocked** by W5. The streams are
orthogonal:

- W5 lives entirely under `swarm::brain`, `swarm::agent_registry`,
  `commands::mailbox`, `commands::swarm`, `swarm::coordinator::fsm`
  (deleted at end).
- W3 backlog lives under `mcp::`, `sidecar::`, `commands::workflows`,
  `agent_runtime/`, `tauri::capabilities`, `tauri::bundle`.
- W4 follow-ups live under `swarm::help_request`,
  `commands::mailbox` (UI tab), `swarm::agent_registry` (per-event
  SQLite).

The W4 follow-up "Swarm comms tab" overlaps with W5-04's job-state
projection — the projector's mailbox subscription naturally
produces the data the comms tab would render. The orchestrator
decides at W5-04 authoring time whether to bundle the UI tab into
W5-04 or keep it as a separate post-W5 polish item. Default: keep
separate, as the projector ships the *data*; a dedicated tab is a
*UI presentation* concern.

Per the 2026-05-09 owner directive ("Owner directive kısmında
dediğim şekilde projeyi ilerletmeye devam edelim"), W5 is the
explicit next direction. W3 backlog + W4 follow-ups stay open
for after W5 closes.

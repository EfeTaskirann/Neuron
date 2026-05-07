---
id: WP-W4-overview
title: Week 4 — Persistent visible swarm (BridgeSwarm-shape rebuild on W3 substrate)
owner: orchestrator
status: planning
---

# Week 4 — Master plan

This document is the planning companion to the per-WP files
(`WP-W4-01-*` … `WP-W4-07-*`). It captures the scope, dependency
graph, and rationale that the individual WPs reference. Per
`AGENTS.md`, each per-WP file is the contract a sub-agent works
against; this file is **not** a contract — it is the orchestrator's
map of how the W3-shipped 9-agent vision is being upgraded from
"hidden one-shot subprocess per stage" to "persistent visible
session per agent + Coordinator-mediated inter-agent comms".

## Source of scope

Every Week 4 line item is tracked back to one of:

- **Owner directive 2026-05-07** — verbatim:
  > "ben her ajanın görünmez bir subprocess olmasını istemiyorum
  > her biri birer terminalde tek başına çalışan olarak çalışıcak.
  > Aynı zamanda birbirleriyle iletişim de kurabilecek."
  Followed by four architectural decisions (see §"Owner decisions
  resolved" below) that pin the W4 shape: persistent (1B), 3×3
  grid (2), Coordinator hub (3C), FSM stays (4A).
- **WP-W3-overview.md** — the W3 backlog whose dependency edges
  the W4 work either inherits or reroutes. W4 does NOT touch the
  W3-04/05/06/07/08/09/10 backlog; those keep their own owner
  decisions.
- **`report/Neuron Multi-Agent Orchestration` architectural report**
  — the original BridgeSwarm-shape vision (visible terminal per
  agent, mailbox-mediated comms) that W3 deferred to ship a working
  one-shot substrate first. W4 is the deferred half made real.
- **`AGENT_LOG.md`** — the 2026-05-07 smoke-test pass entry that
  marks the W3 9-agent vision as PRODUCTION-READY at the FSM /
  persistence / chat-UI level. W4 inherits a green W3.

If a Week 4 item appears here without one of those sources, it is
a scope addition and must be approved by the owner before the
matching WP file is authored.

## Status

| ID | Title | Owner | Status | Blocked by | Size |
|---|---|---|---|---|---|
| WP-W4-01 | `PersistentSession` transport (alongside `SubprocessTransport`) | TBD | not-started | — | M |
| WP-W4-02 | Workspace-scoped agent registry + lazy spawn lifecycle | TBD | not-started | WP-W4-01 | M |
| WP-W4-03 | Per-agent event channel (`swarm:agent:{id}:event`) | TBD | not-started | WP-W4-02 | S |
| WP-W4-04 | `AgentPane` component + 3×3 grid (`SwarmAgentGrid`) | TBD | not-started | WP-W4-03 | M |
| WP-W4-05 | Coordinator hub messaging + `neuron_help` request contract | TBD | not-started | WP-W4-02, WP-W4-03 | M |
| WP-W4-06 | FSM persistent-transport adapter + help-request branch | TBD | not-started | WP-W4-01, WP-W4-05 | M |
| WP-W4-07 | Mailbox swarm tab + observability footer | TBD | not-started | WP-W4-05 | S |

Sizes (rough, in sub-agent days): S = 0.5–1 day, M = 1–2 days,
L = 3+ days. None of the W4 sub-WPs are sized L; this is the
upper bound of "fits in one sub-agent kickoff" intentionally, so
each WP can ship independently with a green-test gate.

## Dependency graph

```
WP-W4-01 (PersistentSession transport)
   │
   ├──► WP-W4-02 (workspace-scoped agent registry + lazy spawn)
   │       │
   │       ├──► WP-W4-03 (per-agent event channel)
   │       │       │
   │       │       └──► WP-W4-04 (AgentPane + 3×3 SwarmAgentGrid)
   │       │
   │       └──► WP-W4-05 (Coordinator hub messaging + neuron_help) ──► WP-W4-07 (mailbox swarm tab)
   │                       │
   │                       ▼
   └──► WP-W4-06 (FSM persistent-transport adapter + help-request branch)
```

W4-04 depends on W4-03 (not W4-02 directly) because the grid is a
pure consumer of the per-agent event channel — once that channel
ships, the UI work parallelises with the rest of the backend. W4-06
depends on both W4-01 (the transport itself) and W4-05 (the
help-request contract that the FSM has to branch on); the WP
authored last absorbs the integration cost.

## Per-WP scope rationale

### WP-W4-01 — `PersistentSession` transport

Today's `swarm::transport::SubprocessTransport` (W3-11) is one-shot:
each `invoke` spawns `claude`, sends one stream-json `user` message,
reads until the `result` event, kills the child. Cold-start is
30–60s on Windows AV first-spawn (per W3-12 history).

W4-01 adds a sibling implementation, `PersistentSession`, alongside
the one-shot. Same `Transport` trait method (`invoke`), but the
child outlives the call: stdin and stdout pipes are held in the
session value, the read loop multiplexes events per turn, the
process is killed only when the session is dropped (workspace
close).

Two transports coexist deliberately:
- **`SubprocessTransport`** — kept for unit tests + the per-invoke
  Orchestrator decide IPC, which is naturally one-shot (one chat
  message in, one outcome out, no continuation expected).
- **`PersistentSession`** — drives the FSM's specialist stages and
  the Coordinator brain. Both want long-lived context for the
  whole job.

Scope:
- New `PersistentSession` struct with `invoke(user_message,
  timeout) -> Result<InvokeResult>` method that writes one turn,
  reads to next `result` event, leaves the process alive
- Lifecycle methods: `spawn(profile)`, `cancel_current_turn()`,
  `shutdown()`
- Per-turn cancel via the existing `tokio::sync::Notify` pattern
  from W3-12c (cancel the read await without killing the child)
- Tests: mock multi-turn round-trip (stub stdin/stdout pair), 20+
  turns in a single session without leak
- Real-claude integration smoke (`#[ignore]`d): two-turn session,
  second turn references content from the first

**Source**: Owner directive 2026-05-07 §1B + W3-11 transport.

### WP-W4-02 — Workspace-scoped agent registry + lazy spawn

W3 spawned a fresh subprocess per stage, so "agent lifecycle" was
a non-concept. W4 introduces persistent sessions, which means
someone has to track the 9 sessions per workspace.

Scope:
- `SwarmAgentRegistry` keyed by `(workspaceId, agentId)`. Holds
  `PersistentSession` plus per-agent metadata (status enum, last
  activity ms, turns taken, cumulative cost).
- **Lazy spawn**: registry is empty when the workspace opens.
  - Orchestrator session spawns on first chat message.
  - Coordinator + 7 specialist sessions spawn on first `dispatch`
    outcome (so users that only chat with Orchestrator never burn
    the other 8 spawns).
- **Eager kill**: workspace close (Tauri window close OR explicit
  user "End swarm" action) calls `shutdown()` on every session.
  The Orchestrator session shuts down too — there's no
  multi-workspace persistence in scope (one workspace per app
  install, per W3-14 §2; multi-workspace UX is post-W4).
- **App restart**: sessions are in-memory. New app launch = new
  sessions. The persisted Orchestrator chat history (W3-12k2)
  re-seeds context as it does today; specialist context resets.
- Status enum: `Idle / Spawning / Running / WaitingOnCoordinator
  / Blocked / Crashed`.
- IPC: `swarm:agents:list_status(workspaceId) -> Vec<AgentStatus>`
  for the grid header.

**Source**: Owner directive 2026-05-07 §1B (lifecycle horizon
= workspace open) + W3-12k2 chat history precedent.

### WP-W4-03 — Per-agent event channel

The W3 substrate emits `swarm:job:{id}:event` (job-scoped). W4 needs
agent-scoped events because each pane subscribes to one agent's
output stream, not a job stream.

Scope:
- New Tauri event channel per agent: `swarm:agent:{id}:event`
- Event variants:
  - `Spawned { profile_id }` — registry just spawned the session
  - `TurnStarted { turn_index }` — registry wrote a new user message
  - `AssistantText { delta }` — chunk of streaming model output
  - `ToolUse { name, input_summary }` — visible tool execution
    (so the user can see "Scout is reading
    `app/src/components/SwarmJobList.tsx`")
  - `Result { outcome }` — turn finished
  - `HelpRequest { reason, question }` — specialist emitted the
    `neuron_help` block (W4-05)
  - `Idle` — turn done, awaiting next dispatch
  - `Crashed { error }` — session died unrecoverably; registry will
    respawn on next dispatch (with a turn-zero context replay)
- Specta-typed payload registered explicitly (same pattern as
  `SwarmJobEvent`, since events are a side channel).
- Frontend listener hook: `useAgentEvents(agentId)` returns a
  ring-buffered tail of recent events (cap 200 per agent).

**Source**: W3-12c streaming-event precedent + Owner directive 2026-05-07
§1B "neler yaptığını canlı olarak görüntülemiş olurum".

### WP-W4-04 — `AgentPane` + 3×3 grid

The visible UI. 9 panes, fixed slot positions:

```
[Orchestrator]  [Coordinator]  [Scout         ]
[Planner    ]  [BackBuilder]  [FrontBuilder]
[BackReviewer] [FrontReviewer][IntegrationTester]
```

Slot order rationale: top row is the routing brain (Orchestrator)
+ orchestrator-of-specialists (Coordinator) + the investigator
(Scout) — these three usually run first in any job, so they belong
on the top visible row. Middle row is the two builders side-by-
side so a Fullstack parallel run reads left-to-right.  Bottom row
is the verification trio (Reviewers + Tester).

Scope:
- New `SwarmAgentGrid` route component. Layout = CSS grid,
  `grid-template-columns: repeat(3, 1fr); grid-template-rows:
  repeat(3, 1fr);`. Gap, OKLCH borders, status-tinted shadows
  (per Charter §"Hard constraints" #4).
- New `AgentPane` component:
  - Header: persona avatar + name + status pill (idle/running/
    waiting/blocked) + turns counter + ⚙ menu (manual interrupt,
    show full transcript)
  - Body: structured event-driven transcript (NOT xterm —
    stream-json is already parsed; round-tripping through ANSI
    is wasted work). User-message bubbles + assistant-text
    streams + tool-use indicators + help-request highlights.
  - Footer: mailbox sender count + cost-so-far + last-activity-ms
- Replaces (does not delete) the W3-12k3 `OrchestratorChatPanel`
  + `SwarmJobList` + `SwarmJobDetail` triple. The W3 components
  remain for the Job-history surface (Recent jobs tab); the new
  grid is the live runtime view. Two surfaces, two purposes.
- Tests: 14+ vitest cases covering each event variant's render,
  focus / a11y on the grid, slot mapping correctness for all 9
  agents.
- Charter §"Hard constraints" #4 compliance: OKLCH only.

**Source**: Owner directive 2026-05-07 §2 (3×3 grid).

### WP-W4-05 — Coordinator hub messaging + `neuron_help` contract

The piece that makes the swarm collaborative instead of just
visible. Specialists today are dead-ends: stage finishes, FSM
moves on. W4-05 lets a specialist say "I need help" and routes
the request through the Coordinator brain.

Scope:
- **Persona contract update** (`backend-builder.md`,
  `frontend-builder.md`, `backend-reviewer.md`,
  `frontend-reviewer.md`, `integration-tester.md`,
  `planner.md`, `scout.md`): each specialist persona body gains
  a "Yardım iste" section:

  > Bir blocker'a takılırsan (eksik context, belirsiz spec,
  > kurtaramadığın tool hatası) tahminle ilerleme. Tek bir fenced
  > JSON block çıkar ve dur:
  > ```json
  > {"neuron_help": {"reason": "...", "question": "..."}}
  > ```
  > Coordinator yanıtlayacak.

- **Backend parser**: after each specialist turn, scan
  `assistant_text` for a `neuron_help` JSON block (defense-in-depth
  4-step parser, same shape as W3-12d Verdict parser). On hit:
  - Set agent status → `WaitingOnCoordinator`
  - Forward the request as a system-style turn to Coordinator's
    session: "Specialist X is blocked. Reason: ..., Question: ...
    What should they do?"
  - Coordinator returns one of three structured outcomes:
    `DirectAnswer { answer }`, `AskBack { followup_question }`
    (Coordinator wants more info from the specialist before
    answering — routes back to specialist), or
    `Escalate { user_question }` (Coordinator wants to ask the
    user — surfaces in the Orchestrator chat panel as a
    Clarify-shape message).
- **Specialist resume**: Coordinator's `DirectAnswer` or
  `AskBack` outcome is fed into the specialist's session as a
  new turn ("Coordinator says: ...") so the specialist resumes
  with the answer in context. Status flips back to `Running`.
- **Mailbox row** for every leg of the conversation
  (specialist→coordinator, coordinator→specialist) so the
  observability tab (W4-07) can render the trace.
- Tests: parser unit tests (8+ shape variants including
  malformed), FSM-with-help-branch tests (specialist→help→answer
  →resume happy path + Escalate path).

**Source**: Owner directive 2026-05-07 §3C verbatim:
> "işler paylaştırıldıktan sonra koordinatör sadece bekleme
> moduna geçicek bu sırada ajanlar bir sorun yaşadığında bunu
> koordinatöre söylerler koordinatör durumu değerlendirip ek
> olarak soru da sorabilir."

### WP-W4-06 — FSM persistent-transport adapter

Wires the existing `CoordinatorFsm` (W3-12a → W3-12k1) to the W4-01
persistent transport instead of one-shot subprocesses, and adds the
help-request branch to the state machine.

Scope:
- `CoordinatorFsm::run_job` resolves the per-stage transport from
  the agent registry (W4-02) instead of constructing a fresh
  `SubprocessTransport`. Same trait method, same return shape; just
  different Transport impl behind the call.
- New FSM transition: `Running → Blocked → CoordinatorQA → Running`.
  Driven by W4-05's help-request hook.
- Existing FSM transitions unchanged (Init → Scout → Classify →
  Plan → Build → Review → Test → Done | Failed). No retry-loop
  changes — verdict-based retry (W3-12e) keeps working as-is.
- Backwards compatibility: a `transport_kind` argument lets call
  sites opt into one-shot for tests and persistent for production.
  Existing W3 tests keep using the one-shot path.
- Real-claude integration smoke: full chain end-to-end on persistent
  sessions, with at least one stage emitting a `neuron_help` block
  to exercise the new branch.

**Source**: Owner directive 2026-05-07 §4A (FSM stays).

### WP-W4-07 — Mailbox swarm tab + observability footer

The existing `commands::mailbox` surface gets a `kind: 'swarm'`
filter so the Coordinator↔specialist traffic from W4-05 is
inspectable.

Scope:
- Backend: existing mailbox table gains a `kind` column (or a
  reserved `from`/`to` namespace) so swarm-internal messages can
  be filtered out of the user-facing mailbox.
- Frontend: new "Swarm comms" tab in the AgentPane footer (or a
  dedicated route slot) showing the message trace per active job.
  Each row: timestamp, from agent, to agent, message type
  (HelpRequest / DirectAnswer / AskBack), short summary.
- AgentPane footer counter: "12 messages from Coordinator" badge,
  click-through to the trace view.

**Source**: Owner directive 2026-05-07 §3C (Coordinator hub
implies a visible trace) + W3 mailbox precedent.

## Authoring sequence

The orchestrator authors per-WP files (`WP-W4-NN-*.md`) on demand,
not all up-front. Each WP file is written immediately before its
sub-agent kickoff, with the latest state of the codebase in
context. This document is the "what's next" reference — it is
allowed to drift slightly from per-WP files as scope is
discovered, but never silently: the diff is logged in
`AGENT_LOG.md` under "scope amendment".

Recommended sequence:

1. **W4-01** (transport) — pure backend, no UI, fully testable
   with mocks. Lands first so the rest can build on it.
2. **W4-02** (registry + lazy spawn) — depends on W4-01;
   integrates with Tauri app state and lib.rs setup. Adds the
   `swarm:agents:list_status` IPC.
3. **W4-03** (event channel) — depends on W4-02;
   small WP, mostly typed event shapes + emit hooks. Frontend
   listener hook lands too.
4. **W4-04** (AgentPane + 3×3 grid) — depends on W4-03; the
   visible payoff. Mostly frontend.
5. **W4-05** (Coordinator hub + neuron_help) — depends on W4-02
   and W4-03. The substantive collaboration layer.
6. **W4-06** (FSM adapter) — depends on W4-01 and W4-05.
   Integration WP; small code, big test surface (every existing
   integration smoke gets a persistent-transport variant).
7. **W4-07** (mailbox swarm tab) — depends on W4-05.
   Observability polish; can ship in parallel with W4-06.

Parallelisation: W4-04 (UI) and W4-05 (backend collaboration)
can run in parallel after W4-03 lands. W4-07 can run in parallel
with W4-06 after W4-05 lands.

## Out of scope (Week 4)

Lifted from the owner directive + Charter; restated here so per-WP
authors don't re-litigate:

- ❌ **Multi-workspace** — one workspace per app install stays
  the rule. Multi-workspace agent isolation, multi-workspace
  registry sharding, etc. are post-W4.
- ❌ **Cross-app-restart session persistence** — sessions are
  in-memory. Killing the app kills all 9 sessions. The chat
  history (W3-12k2) survives because it's SQLite-backed; the
  specialist context does not. (A future WP could snapshot
  context to disk; not in W4 because the wall-time win is
  small and the corruption surface is large.)
- ❌ **User-driven agent selection** — the FSM still picks who
  runs next based on scope/route. A "manually invoke X" surface
  is post-W4.
- ❌ **Multi-job concurrency in a single workspace** — the
  workspace lock from W3-12a stays. Two `swarm:run_job` calls
  with the same workspace still serialise. The 3×3 grid shows
  only the current job's activity.
- ❌ **Specialist-to-specialist direct comms** — by owner
  directive, all inter-agent traffic goes through Coordinator.
  No A→B without B→Coordinator→A round-trip.
- ❌ **Custom user-defined personas** — the 9 bundled `.md`
  files stay the registry. Workspace overrides via
  `<app_data_dir>/agents/*.md` continue to work (W3-11 §2);
  no UI for editing them in W4.
- ❌ **Tab/dock UI for hidden agents** — owner picked grid
  layout #2 (3×3, all visible). No collapse or hide controls
  in W4.

## Owner decisions (resolved)

Recorded here so per-WP authors don't re-litigate. Date noted on
each because Week 4 scope is allowed to evolve as we learn —
re-opening any of these is a scope amendment that lands in
`AGENT_LOG.md`.

1. **Session lifecycle** (resolved 2026-05-07, decision 1B):
   Persistent sessions, alive while the workspace is open. Spawned
   lazily on first dispatch (Orchestrator alone for chat-only
   sessions). Killed on workspace close.

   Trade-off acknowledged: each session's stream-json context
   accumulates over the workspace's lifetime. To bound the growth
   we'll add a turn-count cap per session (default 200; tunable
   via `NEURON_SWARM_AGENT_TURN_CAP`); on cap-hit the registry
   gracefully respawns the session and replays the last N system
   facts (job ids run, last verdict, last 5 mailbox entries).
   Cap + replay is a W4-02 sub-task, not a separate WP.

2. **Layout** (resolved 2026-05-07, decision 2):
   3×3 grid, all 9 panes visible at once. Slot mapping (top-down,
   left-to-right) is fixed in the W4-04 acceptance test list:
   Orchestrator / Coordinator / Scout → Planner / BackendBuilder /
   FrontendBuilder → BackendReviewer / FrontendReviewer /
   IntegrationTester.

3. **Inter-agent communication** (resolved 2026-05-07, decision 3C):
   Coordinator hub. After dispatch, Coordinator goes idle.
   Specialists facing a blocker emit a `neuron_help` JSON block;
   the registry routes it to Coordinator, which decides
   `DirectAnswer / AskBack / Escalate`. No specialist-to-specialist
   direct comms.

   This is more conservative than the BridgeSwarm report's
   "everyone subscribes to mailbox" design. The trade-off is
   simpler debugging (single chokepoint = single trace) at the
   cost of Coordinator latency on the hot path. Owner picked
   simplicity; agreed.

4. **FSM presence** (resolved 2026-05-07, decision 4A):
   The W3 `CoordinatorFsm` (Init → Scout → Classify → Plan →
   Build → Review → Test) stays. W4 only changes the transport
   under the hood (one-shot → persistent) and adds the
   help-request branch to the state machine.

   The "fully autonomous mailbox-driven swarm" alternative
   (decision 4B) is rejected for W4. It's a strictly larger
   refactor and the deterministic FSM has shipped value (see
   W3 acceptance gates). If after W4 the team wants to relax
   the FSM into a full message-bus, that's a future WP-W5.

## Open questions (still gating)

None gating per-WP authoring as of 2026-05-07. The four owner
decisions resolve the architectural shape; per-WP authors handle
the details inside their scopes.

Possible follow-up questions that may surface during W4-04 (UI)
authoring:

- **Pane reordering**: should the user be able to drag-swap slots,
  or is the fixed mapping permanent? (Default: fixed. Drag-swap is
  a polish item that can ship in a follow-up.)
- **Pane focus mode**: clicking a pane could expand it to fullscreen
  with the other 8 collapsed to a sidebar. (Default: not in W4. The
  ⚙ menu's "show full transcript" already covers the deep-dive
  use case.)

These get resolved at W4-04 authoring time, not now.

## Relationship to W3 backlog

The W3 backlog (W3-04 LangGraph cancel, W3-05 approval UI,
W3-08 multi-workflow editor, W3-09 capabilities + E2E,
W3-10 Python embed) is **not blocked** by W4. The two streams are
orthogonal:

- W4 lives entirely under `swarm::` and `app/src/components/Swarm*`.
- W3 backlog lives under `mcp::`, `sidecar::`, `commands::workflows`,
  `agent_runtime/`, `tauri::capabilities`, `tauri::bundle`.

Per the 2026-05-07 owner directive ("WP-W4 önce mi, yoksa W3
backlog'u temizleyip ondan sonra mı W4?" — answered: W4 first),
W3 backlog stays open and is picked up after W4 closes. The
dependency edge from W3-04 to W4 is **none** in either direction.

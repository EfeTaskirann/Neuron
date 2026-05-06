---
id: WP-W3-overview
title: Week 3 — MCP & telemetry hardening, planning overview
owner: orchestrator
status: planning
---

# Week 3 — Master plan

This document is the planning companion to the per-WP files
(`WP-W3-01-*` … `WP-W3-10-*`). It captures the scope, dependency
graph, and rationale that the individual WPs reference. Per
`AGENTS.md`, each per-WP file is the contract a sub-agent works
against; this file is **not** a contract — it is the orchestrator's
map of how the per-WP scopes were carved out of the deferred-to-
Week-3 backlog.

## Source of scope

Every Week 3 line item below is tracked back to one of:

- `PROJECT_CHARTER.md` — Phase row + "Out of scope" + Risks tables
- `docs/work-packages/WP-W2-*.md` — `## Out of scope` sections
  (each WP names what it did NOT ship)
- `tasks/refactor-v1.md` — "Çözüm parçası uygulanan" + "Bu turda
  ertelenen" sections (10 deliberately-deferred items)
- `tasks/report-29-04-26.md` — Y20 (`tokio::time::timeout` cancel
  safety) and other open bugs deferred behind larger refactors
- `NEURON_TERMINAL_REPORT.md` — pane-level Week 3 follow-ups
- `AGENT_LOG.md` — "next" lines naming the next-in-line WPs
- `tasks/agent-briefs-2026-04-29.md` — known caveats handed off to
  Week 3 (e.g. G2 default_installed mismatch)

If a Week 3 item appears here without a Week-2 source, it is a
scope addition and must be approved by the owner before the
matching WP file is authored.

## Status

| ID | Title | Owner | Status | Blocked by | Size |
|---|---|---|---|---|---|
| WP-W3-01 | OS keychain + settings table | TBD | not-started | — | S |
| WP-W3-02 | MCP session pool + cancel safety | TBD | not-started | WP-W3-01 | M |
| WP-W3-03 | MCP install UX — 5 stub manifests fully wired | TBD | not-started | WP-W3-02 | M |
| WP-W3-04 | Agent runtime — cancel propagation + streaming | TBD | not-started | WP-W3-01 | M |
| WP-W3-05 | Approval UI (regex placeholder → real flow) | TBD | not-started | WP-W3-04 | M |
| WP-W3-06 | Telemetry export (OTel collector + sampling) | TBD | not-started | — | M |
| WP-W3-07 | Pane aggregates from spans | TBD | not-started | WP-W3-06 | S |
| WP-W3-08 | Multi-workflow editor + fixture system | TBD | not-started | — | L |
| WP-W3-09 | Capabilities tightening + E2E (Playwright) | TBD | not-started | 02,03,04,06,08 | M |
| WP-W3-10 | PyOxidizer Python embed | TBD | not-started | (W3-04 deferred — see Owner decision #4) | L |
| WP-W3-11 | Swarm runtime foundation (claude subprocess substrate) | done (`f1596f8`) | shipped 2026-05-05 | WP-W3-01 | M |
| WP-W3-12a | Coordinator FSM skeleton (in-memory, blocking, 3-state happy path) | done (`5890841`) | shipped 2026-05-05 | WP-W3-11 | M |
| WP-W3-12b | Coordinator FSM — SQLite persistence + restart recovery | done (`9f8b4de`) | shipped 2026-05-06 | WP-W3-12a | M |
| WP-W3-14 | Swarm UI route (chat-shape, recent-jobs panel, cancel/rerun) | TBD | not-started | WP-W3-12a/b/c | M |
| WP-W3-12c | Coordinator FSM — streaming Tauri events + cancel mid-job (backend only; React hook → W3-14) | done (`3cb6be1`) | shipped 2026-05-05 | WP-W3-12a | M |
| WP-W3-12d | Coordinator FSM — REVIEW + TEST states + Verdict schema + robust JSON parser (NO retry, NO Coordinator brain) | done (`ed98cf5`) | shipped 2026-05-06 | WP-W3-12a/b/c | M |
| WP-W3-12e | Coordinator FSM — retry feedback loop (`MAX_RETRIES=2`, Verdict.rejected → Planner with feedback) | done (`d5e4500`) | shipped 2026-05-06 | WP-W3-12d | M |
| WP-W3-12f | Coordinator FSM — Coordinator LLM brain (Option B: on-demand routing, Classify research/execute) | done (`1ac7347`) | shipped 2026-05-06 | WP-W3-12d/e | M |
| WP-W3-12g | Swarm specialist roster expansion (6 → 8 profiles; backend/frontend split + scope classification) | done (`5f4337a`) | shipped 2026-05-06 | WP-W3-12f | M |
| WP-W3-12h | Coordinator FSM — scope-aware single-domain dispatch (Backend / Frontend) | done (`e0e9f9c`) | shipped 2026-05-06 | WP-W3-12g | M |
| WP-W3-12i | Coordinator FSM — Fullstack sequential dispatch (BB+BR then FB+FR) | TBD | not-started | WP-W3-12h | M |
| WP-W3-12j | Coordinator FSM — Fullstack parallel dispatch (Builder ∥ Builder, Reviewer ∥ Reviewer) | future | not-started | WP-W3-12i | M |
| WP-W3-12k | Orchestrator user-facing chat layer (9th agent: PM dış kapı) | future | not-started | WP-W3-12h+ | L |
| WP-W3-14 | Swarm UI route (chat-shape, recent-jobs panel, cancel/rerun) | done (`2ace648`) | shipped 2026-05-06 | WP-W3-12a/b/c | M |

Sizes (rough, in sub-agent days): S = 0.5–1 day, M = 1–2 days,
L = 3+ days. Anything L is a candidate to split before kickoff.

## Dependency graph

```
WP-W3-01 (keychain + settings)
   │
   ├──► WP-W3-02 (MCP pool + cancel safety) ──► WP-W3-03 (MCP install UX)
   │                                                 │
   ├──► WP-W3-04 (LangGraph cancel + streaming) ──► WP-W3-05 (approval UI)
   │       [DEFERRED 2026-05-05 per Owner decision #4 —
   │        re-evaluate at W3-08 close]               │
   │                                                  ▼
   │   WP-W3-06 (OTel + sampling) ──► WP-W3-07 (pane aggregates)
   │                                                  │
   │                                                  ▼
   │   WP-W3-08 (workflow editor + fixtures)          │
   │                                                  │
   │                                                  ▼
   │                                       WP-W3-09 (capabilities + E2E)
   │                                                  │
   ├──► WP-W3-10 (Python embed) ───── parallel ───────┘
   │       [no longer blocks on W3-04 per Owner decision #4]
   │
   └──► WP-W3-11 (Swarm runtime foundation) ──► W3-12 (Coordinator FSM)
                                              ──► W3-13 (Verdict + retry + broadcast)
                                              ──► W3-14 (Swarm multi-pane UI)
                  [W3-12 / W3-13 / W3-14 are placeholders;
                   per-WP files authored when W3-11 lands]
```

W3-09 is the late-cycle WP because it freezes the command surface
— it must run after every other WP that adds or renames a Tauri
command. W3-10 (PyOxidizer) is parallelizable because it changes
how Python is shipped, not what the sidecar does.

## Per-WP scope rationale

### WP-W3-01 — OS keychain + settings table

Charter §"Hard constraints" #2 ("API keys live in OS keychain.
Never plaintext, never `.env` committed") was honored on the Python
side from day one (`agent_runtime/secrets.py` uses `keyring`). On
the Rust side, MCP secrets read via `std::env::var` (see
`mcp/registry.rs:228-244`). W3-01 closes that gap with a
`crate::secrets` module backed by the `keyring` crate (matching the
Python service name `"neuron"` so Rust and Python share one
keystore).

In the same WP, a `settings` table replaces the hardcoded values
in `commands/me.rs` (Efe Taşkıran / Personal) so the Settings route
(W3-09 era) has somewhere to write user-edited values. This is
deliberately bundled — both touches are small and share the
"Settings route data sources" theme.

**Source**: refactor-v1.md C5 prerequisites; `commands/me.rs:23-30`
TODO; Charter §2.

### WP-W3-02 — MCP session pool + cancel safety

`mcp:callTool` currently re-spawns the server per call (`registry.rs:99-132`).
Cold-start cost for `npx` is 0.5–2s per call, so an agent making 10
tool calls eats 10s of pure spawn overhead.

Two issues fold together:
- **Pool**: a long-lived `McpClient` per installed server, with
  request-id correlation (`mcp/client.rs` already has the
  `next_id` AtomicU64; needs a pending-request map for true
  multiplexing).
- **Cancel safety**: `tokio::time::timeout(read_line)` is not
  cancellation-safe — if the timeout fires mid-frame, the next
  read sees a corrupt stream (Y20 in report-29-04-26.md). This
  is benign per-call today (we drop the client) but breaks any
  pooled session where the next request would inherit the corrupt
  state.

**Source**: WP-W2-05 §"Notes" + refactor-v1.md C5 + report-29-04-26.md Y20.

### WP-W3-03 — MCP install UX (5 stubs → real)

Currently 5 of 12 catalog manifests have `spawn: null`
(`browser`, `slack`, `vector-db`, `linear`, `notion`, `stripe`,
`sentry`, `figma`, `memory` — actually the stub set is the latter
seven plus `postgres`; tally lives in `mcp/manifests.rs:93-110`).
Installing surfaces `mcp_server_spawn_failed`. W3-03 wires real
`npx -y` recipes for each, plus the `default_installed` flag fix
(refactor-v1.md G2: mock has 3 servers pre-installed; backend
ships all 0).

**Source**: WP-W2-05 §"Out of scope" + refactor-v1.md G2.

### WP-W3-04 — Agent runtime cancel + streaming

Two Charter "Out of scope" items from WP-W2-04:
- Cancel signal mid-LLM-call — refactor-v1.md G1 keeps the row-flip
  side closed (`runs:cancel` UPDATE is atomic) but the sidecar still
  finishes the LLM call. W3-04 adds a `cancel_run` frame to the
  Python protocol (`__main__.py`) and drives `asyncio.Task.cancel()`
  inside `_start_run`.
- Streaming partial LLM responses — currently each `span.updated`
  carries the full prompt/response. Streaming means token-level
  `span.updated` deltas, so the inspector can render mid-flight.

**Source**: WP-W2-04 §"Out of scope" + refactor-v1.md G1.

### WP-W3-05 — Approval UI (regex → real)

`sidecar/terminal.rs::extract_approval_blob` ships a placeholder
banner because agent CLIs don't yet emit a stable machine-readable
approval block (sidecar README:115). W3-05 replaces the regex with
a real protocol:
- Wrap each agent CLI in a small "approval shim" that emits
  structured `[NEURON_APPROVAL]{...}` lines on its stdout.
- Reader detects the shim line and parses JSON directly (no regex).
- Frontend "Accept / Reject" buttons (`Terminal.tsx:289-291`)
  actually post the answer back via a new `terminal:approve` /
  `terminal:reject` command.

**Source**: NEURON_TERMINAL_REPORT.md "Week 3 may replace regex
with structured exit codes / stdout markers" + sidecar README L115.

### WP-W3-06 — Telemetry export (OTel collector + sampling)

WP-W2-07 shipped span persistence (SQLite) + run aggregates. W3-06
adds the OTel collector export path: a background task batches
`runs_spans` rows that have `exported_at IS NULL`, posts them as
OTLP to a configurable endpoint (`settings:otel.endpoint` from
W3-01), and updates `exported_at`. Sampling rules live in the same
table; trim policy (delete spans older than N days) is a separate
sweep.

**Source**: WP-W2-07 §"Out of scope".

### WP-W3-07 — Pane aggregates from spans

`Pane.tokensIn` / `tokensOut` / `costUsd` always ship `None`
(Charter §1 carve-out, models.rs:307-313). W3-07 sources them from
`runs_spans` aggregates joined to the pane's owning run(s). Depends
on W3-06 only because the aggregate query needs the span-export
sweep to NOT race the pane read; both share the `runs_spans`
indexes added in W3-06.

**Source**: agent-briefs-2026-04-29.md §B + models.rs comments.

### WP-W3-08 — Multi-workflow editor + fixture system

Charter Phases row: "multi-workflow editor" is explicitly Week 3.
ADR-0007 §"workflows row source" calls out fixture-system
migration. W3-08 is the largest WP — adds a workflow editor route
(create / rename / delete workflows; node + edge editing on the
canvas), plus a fixture loader that replaces `seed_demo_workflow` +
`seed_demo_canvas` (the latter currently inserts six hardcoded
nodes from `db.rs:108-156`).

**Source**: Charter Phases + ADR-0007 + WP-W2-08 §"Risks" (seed
fixture).

### WP-W3-09 — Capabilities tightening + E2E

Two unrelated tail-end concerns folded because both run "after the
command surface is final":
- **Capabilities**: Tauri 2 capabilities are command-bazlı; today
  `capabilities/default.json` allows `core:default`. W3-09 narrows
  this to the actual command list, plus per-window restrictions if
  multi-window lands.
- **E2E**: WP-W2-01 deferred Tauri-window automation (Playwright +
  WebDriver) to Week 3. W3-09 adds **full automation** of the
  WP-W2-08 §"Verification" manual smoke list (all 10 items), per
  the 2026-05-01 owner decision. The "no human runs the smoke
  list ever again" payoff sizes this WP at M; full smoke covers
  every route's golden path so regressions surface before merge,
  not in production.

**Source**: refactor-v1.md E1 + WP-W2-01 §"Out of scope".

### WP-W3-10 — Python embed (python-build-standalone)

Charter Risks table line 1: "Week 2 requires system Python 3.11+;
embed in Week 3". W3-10 swaps `Command::new("python")` for a
bundled interpreter so the desktop installer is self-contained.

Per the 2026-05-01 orchestrator decision, the primary plan is
`python-build-standalone` (the prebuilt CPython tarballs Astral
ships and uv consumes), NOT PyOxidizer. The build flow:

1. `tauri-build` downloads the matching standalone tarball for
   each target triple at bundle time.
2. The Python tree lands under `<bundle>/python/` next to the
   Neuron binary.
3. `sidecar::agent::resolve_python` gains a fourth resolution
   step (after the existing env override / venv / PATH chain):
   the bundled standalone interpreter at the platform-conventional
   relative path. Bundled wins over PATH so a system Python
   mismatch can't break the agent runtime.

This is parallelizable with the rest of Week 3 because it doesn't
change the protocol — only how the Python child is started.

**Source**: Charter Risks (updated by this commit to allow
either embed strategy).

## Authoring sequence

The orchestrator authors per-WP files (`WP-W3-NN-*.md`) on demand,
not all up-front. Each WP file is written immediately before its
sub-agent kickoff, with the latest state of the codebase in
context. This document is the "what's next" reference — it is
allowed to drift slightly from per-WP files as scope is
discovered, but never silently: the diff is logged in
`AGENT_LOG.md` under "scope amendment".

## Out of scope (Week 3)

Lifted from Charter; restated here so per-WP authors don't
re-litigate:

- ❌ Multi-user / cloud sync (post-Week-3)
- ❌ Auth / OAuth passthrough (post-Week-3)
- ❌ Plugin system / third-party MCP marketplace UI
- ❌ Multi-window
- ❌ Mobile / web build targets
- ❌ Anthropic model bumps (handled outside the WP cadence)
- ❌ Husky / pre-commit hook tooling (refactor-v1.md E2 — needs
  its own ADR before it lands)

## Owner decisions (resolved)

Recorded here so per-WP authors don't re-litigate. Date noted on
each because Week 3 scope is allowed to evolve as we learn —
re-opening any of these is a scope amendment that lands in
`AGENT_LOG.md`.

1. **Provider list for W3-01 keychain** (resolved 2026-05-01):
   Ship with `anthropic` + `openai` only. Additional providers
   (`gemini`, `groq`, `together`, …) handled by a future
   follow-up WP that just adds rows in the Settings UI dropdown
   — the `crate::secrets` API is generic over `key: &str` so no
   API change is needed when expanding. Document the two-provider
   default in the WP-W3-01 acceptance test list.

2. **W3-09 E2E coverage** (resolved 2026-05-01):
   Full automation. Every line in WP-W2-08 §"Verification"
   manual-smoke list (10 items) runs as a Playwright + Tauri
   WebDriver test. **Size estimate: S → M** (table updated). The
   doubled cost buys "no human runs the smoke list ever again",
   which is the right trade for a desktop app shipping monthly.

3. **W3-10 Python embed strategy** (resolved 2026-05-01,
   orchestrator's call):
   `python-build-standalone` (the runtime uv ships) is the
   primary plan, **not PyOxidizer**. Rationale:
   - PyOxidizer's last release was 2024-02; maintenance has
     visibly slowed. Betting Week-3 work on it is a trailing-
     edge dependency choice.
   - `python-build-standalone` is actively maintained by Astral
     (uv's owner); we already use uv for the agent_runtime venv
     so the build tooling is in place.
   - Trade-off: bundle size is 50–80 MB larger than a true
     PyOxidizer embed. Given Tauri's ~10 MB baseline this still
     keeps Neuron well under the Electron-equivalent footprint
     (~150 MB+). Acceptable.

   The W3-10 WP file (when authored) will revisit if Astral
   ships a smaller variant by then. Charter Risks table line
   1 is updated in this commit to read "PyOxidizer or
   `python-build-standalone` in Week 3" — no decision lock-in.

4. **Swarm runtime introduction + W3-04 deferral** (resolved
   2026-05-05): a new agent runtime — `claude` CLI subprocess
   pool, see WP-W3-11 — is added alongside LangGraph (NOT
   replacing it). Rationale:
   - LangGraph sidecar continues to power the scripted "Daily
     summary" demo (button-triggered, fixed graph, currently
     the only scripted workflow). Charter Phases row "Week 2
     release gate" depends on it; we don't break that.
   - Swarm runtime serves a different feature shape:
     chat-triggered, Coordinator-decided multi-agent flow with
     `.md` profile-driven specialists. The user-facing pitch is
     "şefli ekip", not "scripted workflow". Different shape
     means different runtime.
   - The two runtimes share SQLite (runs/spans tables) but never
     cross-import. Phase 1 substrate (WP-W3-11) explicitly
     forbids the import — recorded in §"Sub-agent reminders".
   - **W3-04 deferred** (LangGraph cancel + streaming): priority
     drops because (a) Swarm gets cancel + streaming as a first-
     class concern via W3-12+, (b) Daily summary is a 30-second
     scripted job — no user-facing demand for cancel yet, (c)
     only one scripted workflow exists; cancel ergonomics matter
     once W3-08 (workflow editor) ships and a non-trivial pile
     of long-running workflows can be authored. **Re-evaluate
     W3-04 at W3-08 close.** The status table above marks it
     `not-started` (unchanged) but its dependency edge into
     W3-10 is broken.
   - **W3-10 reframed**: no longer blocks on W3-04 — Python
     embed work proceeds independently so the bundle stays
     self-contained even if W3-04 sleeps long-term. The
     dependency graph above is updated.

   The architectural ground truth for Swarm is the
   `report/Neuron Multi-Agent Orchestration` report and
   WP-W3-11. Future swarm WPs (W3-12 Coordinator FSM, W3-13
   verdict + retry + broadcast, W3-14 multi-pane UI) reference
   them.

## Open questions (still gating)

4. **W3-08 scope on canvas editing**: single-node add / edit /
   delete / rename + run only (M-sized), or also multi-select
   (shift-click, marquee, Ctrl+A) and undo/redo (L+)? Multi-select
   pays off once user workflows exceed ~10 nodes. Owner deferred
   the call until W3-08 authoring is closer.

The remaining question gates per-WP authoring for W3-08 only.
The orchestrator can ship W3-01 → W3-07 and W3-09/10 without it.

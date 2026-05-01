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
| WP-W3-09 | Capabilities tightening + E2E (Playwright) | TBD | not-started | 02,03,04,06,08 | S |
| WP-W3-10 | PyOxidizer Python embed | TBD | not-started | WP-W3-04 | L |

Sizes (rough, in sub-agent days): S = 0.5–1 day, M = 1–2 days,
L = 3+ days. Anything L is a candidate to split before kickoff.

## Dependency graph

```
WP-W3-01 (keychain + settings)
   │
   ├──► WP-W3-02 (MCP pool + cancel safety) ──► WP-W3-03 (MCP install UX)
   │                                                 │
   └──► WP-W3-04 (agent cancel + streaming) ──► WP-W3-05 (approval UI)
                                                 │
                                                 ▼
WP-W3-06 (OTel + sampling) ──► WP-W3-07 (pane aggregates)
                                                 │
                                                 ▼
WP-W3-08 (workflow editor + fixtures)            │
                                                 │
                                                 ▼
                                       WP-W3-09 (capabilities + E2E)
                                                 │
WP-W3-10 (PyOxidizer) ──── parallel ─────────────┘
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
  WebDriver) to Week 3. W3-09 adds a smoke E2E that exercises the
  manual smoke list in WP-W2-08:Verification.

**Source**: refactor-v1.md E1 + WP-W2-01 §"Out of scope".

### WP-W3-10 — PyOxidizer Python embed

Charter Risks table line 1: "Week 2 requires system Python 3.11+;
PyOxidizer in Week 3". W3-10 swaps `Command::new("python")` for an
embedded interpreter so the desktop bundle is self-contained. This
is parallelizable with the rest of Week 3 because it doesn't change
the protocol — only how the Python child is started.

**Source**: Charter Risks.

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

## Open questions (need owner answer before kickoff)

1. **Provider list for W3-01 keychain**: just `anthropic` + `openai`
   per the existing Python sidecar, or also `gemini` /
   `groq` / `together` so the agent runtime can swap providers
   without a re-keying step?
2. **W3-08 scope on canvas editing**: edge-add and node-add only,
   or full undo/redo + multi-select? Affects size estimate (M vs L+).
3. **W3-09 E2E coverage breadth**: smoke (one happy path per
   route) or full WP-W2-08 §10 manual smoke list automated? The
   second roughly doubles the WP size.
4. **W3-10 PyOxidizer vs alternatives**: PyOxidizer's
   maintenance status has been wobbly. Is `python-build-standalone`
   (used by uv) acceptable as a fallback? Cheaper; loses true
   single-binary embed.

These four answers gate the per-WP authoring; the orchestrator
will stop and ask before writing the affected WP file.

---
id: WP-W2-07
title: Span / trace persistence
owner: TBD
status: not-started
depends-on: [WP-W2-04]
acceptance-gate: "Run inspector renders real backend spans; latency/token/cost extracted; live updates work"
---

## Goal

Persist OTel-style spans from the agent runtime (WP-W2-04) into `runs_spans` such that `runs:get(id)` returns spans matching the shape the frontend `inspector.jsx` expects (`id`, `name`, `indent`, `type`, `t0`, `dur`, `attrs`, `prompt`, `response`, `is_running`). Real-time event streaming for live updates while a run is open.

## Scope

- Sidecar (WP-04) emits structured span events:
  ```json
  {"type":"span.created","span":{"id":"s1","parent_id":null,"run_id":"r1","name":"orchestrator.run","span_type":"llm","t0_ms":0,"attrs":{}}}
  {"type":"span.updated","id":"s1","attrs":{"tokens_in":412}}
  {"type":"span.closed","id":"s1","duration_ms":2400}
  ```
- Rust event handler in `src-tauri/src/sidecar/agent.rs`:
  - `span.created` → `INSERT INTO runs_spans (... is_running=1)`
  - `span.updated` → `UPDATE runs_spans SET attrs_json = ?` (merge)
  - `span.closed` → `UPDATE runs_spans SET duration_ms = ?, is_running = 0`
- Indent computed at READ time via recursive CTE on `parent_span_id` depth (NOT stored)
- Run aggregates updated on each span close:
  - `runs.tokens` = SUM(spans.attrs_json -> '$.tokens_in') + SUM(... -> '$.tokens_out')
  - `runs.cost_usd` = SUM(spans.attrs_json -> '$.cost')
  - `runs.duration_ms` = MAX(spans.t0_ms + spans.duration_ms) - MIN(spans.t0_ms)
- Real-time: sidecar→Rust→frontend event chain. Tauri event `run.{id}.span` emitted on every create/update/close.
- Frontend hook `useRun(id)`:
  - Initial fetch via `invoke('runs:get', { id })`
  - Subscribe to `run.{id}.span` while inspector is open; merge updates into local state
  - Unsubscribe on unmount

## Out of scope

- OTel collector export (Week 3)
- Span sampling / trimming (Week 3 if size becomes an issue)
- Cross-run trace stitching (Week 3 — multi-workflow chains)

## Acceptance criteria

- [ ] After a `runs:create` triggers a real run (WP-04), spans land in `runs_spans` within 100ms of sidecar emission
- [ ] `runs:get(id)` returns spans ordered by `t0_ms ASC`
- [ ] Indent in returned spans matches the parent_id tree depth (e.g., `orchestrator.run` indent=0, `llm.plan` indent=1, `logic.route` indent=2)
- [ ] `runs.tokens` and `runs.cost_usd` are populated correctly post-completion
- [ ] Frontend run inspector shows live updates: bar widths grow as `span.updated` arrives; bar locks when `span.closed`
- [ ] Running spans (`is_running=1`) include `wf-shimmer` class on the bar
- [ ] Selected-span sheet shows `prompt` and `response` for `type='llm'` spans
- [ ] Reopening the inspector for a completed run shows the persisted snapshot (no events needed)

## Verification commands

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- spans
# manual:
const { id } = await invoke('runs:create', { workflowId: 'daily-summary' });
// while running, open Run Inspector for `id` → spans appear and update live
// after completion, refresh page → spans still there, no events needed
```

## Notes / risks

- JSON path queries (`json_extract(attrs_json, '$.tokens_in')`) may be slow on large spans. Consider denormalizing `tokens_in/tokens_out/cost_usd` to dedicated columns in WP-W2-07 if perf is bad.
- Recursive CTE for indent: SQLite supports `WITH RECURSIVE`, but be careful with cycles. Spans are guaranteed acyclic by parent_id constraint.
- Event delivery is best-effort (Tauri fires-and-forgets). If a frontend listener attaches AFTER spans started arriving, fall back to refetching via `runs:get`.
- DB write throughput: ~50 spans/run, 1 run/sec at peak — well within SQLite limits. No batching needed.

## Sub-agent reminders

- Do NOT change the span event schema between sidecar and Rust without updating both sides in this WP.
- Do NOT add an OTel exporter in this WP (Week 3).
- Frontend hook (`useRun`) is part of WP-W2-08; this WP only delivers the BACKEND event surface and TypeScript types via bindings regen.

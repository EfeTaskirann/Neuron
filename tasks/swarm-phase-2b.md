# Swarm Runtime — Phase 2b (WP-W3-12b) execution plan

Source: `docs/work-packages/WP-W3-12b-sqlite-persistence.md`.

## Status

- [x] WP-W3-11 / W3-12a / W3-12c — shipped (`f1596f8`,
      `5890841`, `3cb6be1`).
- [x] Owner approval (2026-05-05): "sırasıyla ilerlemeye devam et
      her aşamadan sonra da testi et. Bir problem yoksa sonraki
      aşamaya da geçebilirsin." → autonomous progression through
      W3-12b → W3-14 → W3-12d, orchestrator runs all smokes.

## Decisions baked into WP

1. **`JobRegistry::with_pool(pool)`** — production path.
   `JobRegistry::new()` stays for tests (in-memory only).
2. **All mutators become `async fn`**; reads stay sync.
3. **Write-through, not lazy** — each state change writes inline.
4. **Orphan recovery on startup** flips non-terminal → Failed
   with `last_error = "interrupted by app restart"`.
5. **In-memory cache cap 100 rows post-recovery** — full history
   via `swarm:list_jobs`.
6. **`swarm:list_jobs` + `swarm:get_job` IPC** — first time `Job`
   surfaces in bindings (as `JobDetail`).

## Plan checklist

### A. Planning (orchestrator)

- [x] Author `WP-W3-12b-sqlite-persistence.md`
- [x] Author this file
- [ ] Update `docs/work-packages/WP-W3-overview.md` Status row
      (W3-12c flipped to done, W3-12b in-flight then done)

### B. Sub-agent scope (single-dispatch)

- [ ] Migration `0006_swarm_jobs.sql` (3 tables + 3 indexes)
- [ ] `JobRegistry::with_pool` constructor + async mutators
- [ ] `swarm/coordinator/store.rs` SQL helpers (`pub(super)`)
- [ ] `JobState::{as,from}_db_str` mapping
- [ ] `recover_orphans` + invocation from `lib.rs::setup`
- [ ] `JobSummary` and `JobDetail` IPC types
- [ ] `swarm_list_jobs` and `swarm_get_job` Tauri commands
- [ ] All unit tests per WP §7
- [ ] Integration test (`#[ignore]`)
      `integration_persistence_survives_real_claude_chain`
- [ ] `pnpm gen:bindings` regen
- [ ] All gates self-run

### C. Verification (orchestrator-rerun + smokes)

- [ ] Independent re-run of every gate
- [ ] Three integration smokes:
  - `integration_persistence_survives_real_claude_chain` (new)
  - `integration_fsm_drives_real_claude_chain` (W3-12a regression)
  - `integration_cancel_during_real_claude_chain` (W3-12c regression)

### D. Commit

- [ ] One WP commit + AGENT_LOG follow-up

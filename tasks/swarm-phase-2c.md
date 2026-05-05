# Swarm Runtime — Phase 2c (WP-W3-12c) execution plan

Source: `docs/work-packages/WP-W3-12c-streaming-events-cancel.md`.
Architectural ground truth: `report/Neuron Multi-Agent
Orchestration` §8.3 (Long-Running Task Feedback).

## Status

- [x] WP-W3-11 (Phase 1 substrate) — `f1596f8`
- [x] WP-W3-12a (FSM skeleton) — `5890841`
- [x] Owner approval (2026-05-05): proceed with W3-12c
      (streaming + cancel) before W3-12b (persistence). Backend
      + bindings only; React hook is W3-14.
- [x] Owner directive: orchestrator runs the integration smoke
      tests directly (terminal access proven during W3-12a).

## Decisions baked into WP

1. **Single per-job event channel** with `kind` discriminator
   (matches `runs:{id}:span` from W3-06). Five kinds: Started,
   StageStarted, StageCompleted, Finished, Cancelled.
2. **Cancel via `tokio::sync::Notify`** — no new dep.
   `JobRegistry` gains a `cancel_notifies` map.
3. **`swarm:cancel_job` rejects unknown / terminal jobs** with
   `NotFound` / `Conflict` errors.
4. **FSM swallows emit errors** — long-running job survives a
   transient window-closing emit failure with a `tracing::warn!`.
5. **No frontend code in this WP** beyond `bindings.ts` regen.
   `useSwarmJob` React hook is W3-14.
6. **No token-level streaming.** Stage-level events only.

## Plan checklist

### A. Planning (orchestrator)

- [x] Author `WP-W3-12c-streaming-events-cancel.md`
- [x] Author this file
- [ ] Update `docs/work-packages/WP-W3-overview.md` Status row
      for W3-12c (TBD → in-flight, then done after commit)

### B. Sub-agent scope (single-dispatch general-purpose)

- [ ] `SwarmJobEvent` enum in `swarm/coordinator/job.rs` with
      five kinds (Started/StageStarted/StageCompleted/Finished/
      Cancelled), `specta::Type`'d, `serde(tag = "kind",
      rename_all = "snake_case")`
- [ ] `JobRegistry` gains `cancel_notifies` map +
      `register_cancel`/`unregister_cancel`/`signal_cancel`
- [ ] FSM `run_job` restructured with `tokio::select!` per
      stage; emits 5 event kinds per the WP §3 sequence
- [ ] `finalize_cancelled` helper mirrors `finalize_failed`
      (stamps `last_error = "cancelled by user"`, terminal
      state Failed, releases workspace, fires `Cancelled` event)
- [ ] `swarm_cancel_job` Tauri command in `commands/swarm.rs`,
      registered in `lib.rs::specta_builder_for_export`
- [ ] All 10+ unit tests from WP §6
- [ ] One `#[ignore]`d integration test
      (`integration_cancel_during_real_claude_chain`)
- [ ] `pnpm gen:bindings` regenerates with `swarmCancelJob` +
      `SwarmJobEvent`
- [ ] Self-run all gates

### C. Verification (orchestrator-rerun + smoke)

- [ ] Independent re-run of every gate per AGENTS.md
- [ ] Orchestrator-driven manual integration smoke (per
      2026-05-05 directive): both `integration_cancel_during_*`
      and the existing `integration_fsm_drives_*` from W3-12a
      pass against the real `claude` binary

### D. Commit

- [ ] One WP commit: `feat: WP-W3-12c streaming Tauri events +
      cancel mid-job`
- [ ] AGENT_LOG entry as follow-up commit

## Open questions

None — all decisions baked into the WP per owner approval to
proceed.

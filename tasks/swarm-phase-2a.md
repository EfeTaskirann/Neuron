# Swarm Runtime — Phase 2a (WP-W3-12a) execution plan

Source: `docs/work-packages/WP-W3-12a-coordinator-fsm-skeleton.md`.
Architectural ground truth: `report/Neuron Multi-Agent
Orchestration — Mimari Analiz Raporu` §5 (Coordinator State Machine),
§11.4 (Single Claude Instance vs Multiple Processes).

This file mirrors the `tasks/swarm-phase-1.md` cadence — orchestrator
checklist + post-flight Review.

## Status

- [x] WP-W3-11 (Phase 1 substrate) — landed in commit `f1596f8`,
      logged in commit `893f1f1`.
- [x] Manual mini-flow chain validation (2026-05-05) — three
      direct `claude -p --append-system-prompt-file` calls with
      bundled `scout` / `planner` / `backend-builder` profiles.
      Scout produced format-correct findings (### Bulgular /
      ### Belirsizlikler), Planner produced 2-step atomic plan
      with verification commands, BackendBuilder respected scope
      (Step 1 only) under `--permission-mode plan`. Persona
      hand-off works.
- [x] Architectural Q1 trade-off (resolved 2026-05-05):
      W3-12a → Option A (pure Rust FSM, no Coordinator LLM);
      W3-12d → Option B (on-demand single-shot Coordinator brain);
      W3-13+ → Option C (persistent Coordinator subprocess).
      Reasoning recorded in `WP-W3-12a-coordinator-fsm-skeleton.md`
      §"Architectural rationale".
- [ ] Owner approval: WP-W3-12a scope, Charter (no-op — same
      tech-stack row from W3-11 covers it), dispatch decision.

## Resolved questions (2026-05-05)

1. **Coordinator architecture for 12a** — Option A (pure Rust
   FSM). Smallest validation surface; tests in isolation; trivial
   upgrade path to Option B at W3-12d.
2. **Manual chain verification before WP** — done (2026-05-05).
   Substrate persona-hand-off confirmed; FSM is now codifying a
   chain we know works.
3. **REVIEW / TEST states** — defined in the enum but unreachable
   in 12a (`debug_assert!` guard). W3-12d authors `reviewer.md` /
   `integration-tester.md` profiles + Verdict schema + retry
   feedback loop, all in one bundled WP.
4. **Sub-WP split** — 12a (FSM in-memory blocking) → 12b
   (persistence) → 12c (streaming) → 12d (REVIEW/TEST + Verdict
   + retry + Coordinator brain). Recorded in WP §"Why now".

## Plan checklist

### A. Charter / planning hygiene (orchestrator)

- [x] Author `WP-W3-12a-coordinator-fsm-skeleton.md`
- [x] Author this file
- [ ] Append W3-12a row to `WP-W3-overview.md` Status table
      (`WP-W3-12a | Coordinator FSM skeleton | TBD | not-started |
      WP-W3-11 | M`); also stub-out 12b/12c/12d rows so the
      dependency graph reflects the sub-WP split
- [ ] No Charter amendment — Swarm runtime row from W3-11
      covers FSM mechanics

### B. Sub-agent scope (general-purpose, dispatched after owner approval)

- [ ] `Transport` trait extracted from W3-11's
      `SubprocessTransport`; existing tests retargeted
- [ ] `swarm/coordinator/{mod,fsm,job}.rs` created
- [ ] `JobState`, `Job`, `JobOutcome`, `StageResult` types
      defined and `specta::Type`'d
- [ ] `JobRegistry` in-memory `Arc<Mutex<HashMap>>` impl
- [ ] `CoordinatorFsm::run_job` walks SCOUT → PLAN → BUILD → DONE
      via the trait
- [ ] Stage prompt templates as fixed `const &str` + `format!`
- [ ] Error mapping: stage errors → `JobOutcome.final_state =
      Failed` + `last_error`; IPC always returns `Ok(JobOutcome)`
- [ ] `swarm:run_job` Tauri command + register in
      `lib.rs::specta_builder_for_export`
- [ ] `JobRegistry` `app.manage`d in `lib.rs::setup`
- [ ] 10–14 unit tests using `MockTransport`
- [ ] 1 `#[ignore]`d integration test driving real `claude`
- [ ] `pnpm gen:bindings` regenerates with `swarmRunJob`,
      `JobOutcome`, `JobState`, `Job`, `StageResult`
- [ ] Self-run all gates: `cargo check`, `cargo test --lib`,
      `pnpm typecheck`, `pnpm test --run`, `pnpm lint`,
      `pnpm gen:bindings:check`

### C. Verification gates (orchestrator-rerun, post-sub-agent)

- [ ] Independent re-run of every gate per AGENTS.md
- [ ] Owner-driven manual integration smoke (real `claude`) —
      canonical `profile_count` goal completes with `Done` in
      <180s

### D. Commit

- [ ] One WP commit (per AGENTS.md): `feat: WP-W3-12a Coordinator
      FSM skeleton (in-memory, blocking, 3-state happy path)`
- [ ] AGENT_LOG entry as a follow-up commit (W3-01 / W3-06 /
      W3-11 cadence)

## Dispatch decision (pending)

Default plan: **single sub-agent dispatch** for B–D. Hybrid
dispatch (orchestrator scaffolds, sub-agent fills) made sense for
W3-11 because the module was new and dep choices were
owner-facing. W3-12a is mostly mechanical Rust on top of an
existing module — single sub-agent is the cleaner fit.

Owner can request hybrid if they prefer the orchestrator to
write the `Transport` trait extraction (which IS a public-API
choice on top of W3-11) before dispatch.

## Resolved questions (2026-05-05)

1. **Stage timeout default**. ✅ **60s/stage** (180s total for
   the 3-stage chain). Owner-confirmed.
2. **Concurrency policy**. ✅ **Per-workspace serialization**.
   Same `workspace_id` → second call rejected with
   `AppError::WorkspaceBusy{..}`. Different `workspace_id`s →
   parallel. Owner reasoning: "Aynı proje için yeni bir 9 kişilik
   ekibi çalıştırmama izin vermesin, başka bir proje için izin
   versin." `swarm:run_job` IPC gains a `workspace_id: String`
   parameter (Neuron has no formal multi-workspace UI yet —
   callers pass `"default"` until W3-12c+ frontend hook adds the
   project picker; the IPC contract is forward-compatible).
3. **9-agent bundle approach**. ✅ **Kademeli (gradual)**.
   W3-12a stays with the 3 bundled profiles from W3-11
   (scout/planner/backend-builder). REVIEW/TEST profiles arrive
   in W3-12d, frontend variants in W3-13+. Bundling all 9 now
   risks persona-FSM divergence (writing personas before their
   state-machine context is clear).
4. **Dispatch**. ✅ **Single sub-agent** (default). Hybrid
   declined.

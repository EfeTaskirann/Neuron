---
id: WP-W5-06
title: FSM deprecation + 435-test migration + final integration smoke (autonomous swarm ships)
owner: TBD
status: not-started
depends-on: [WP-W5-04, WP-W5-05]
acceptance-gate: "All FSM-internal unit tests deleted; lifecycle + integration tests rewritten against the brain dispatcher; `coordinator::fsm` module deleted; `swarm:run_job` IPC redirects to the brain dispatcher (renamed from `swarm:run_job_v2`); the W3-12 real-claude integration battery passes against the brain dispatcher (research-only / single-domain backend / single-domain frontend / fullstack). `cargo test --lib` ≥ 350 passing tests (was 435; ~85 net deletions); `pnpm gen:bindings:check` / `typecheck` / `lint` / `test` green; AGENT_LOG.md captures the test-count delta + deleted-LOC count."
---

## Goal

Remove the W3 FSM. By end of W5-06:

- `src-tauri/src/swarm/coordinator/fsm.rs` is deleted (7322
  lines).
- `src-tauri/src/swarm/coordinator/` retains only the still-used
  modules: `decision.rs` (Coordinator brain decision parser
  W3-12f, repurposed for brain Classify hints), `verdict.rs`
  (Reviewer/Tester verdict parser, used by the brain),
  `orchestrator.rs` + `orchestrator_session.rs` (W3-12k1+k2).
- `swarm:run_job` IPC drives the brain dispatcher (renamed from
  the W5-03 `swarm:run_job_v2`). The IPC signature is unchanged
  so the frontend keeps working.
- The 435 → ~350 Rust unit tests cover the brain dispatcher,
  projector, dispatcher, and bus end-to-end.
- The W3-12 real-claude integration battery passes on the brain.

This is the L-sized teardown WP. Per the W5-overview's escape
hatch, the sub-agent MAY split this into W5-06a (test migration,
FSM stays alive) + W5-06b (FSM deletion). The split point is the
green-test-gate at the end of test migration.

## Why now

Owner directive 2026-05-09 §1: relax the FSM into a fully
autonomous mailbox-driven swarm. W5-01..05 shipped the substrate;
W5-06 closes the directive by deleting the FSM. After W5-06 the
project's only dispatch path is the brain.

## Charter alignment

- **Tech stack**: no new dependency. Just deletions + signature
  routing.
- **Frontend mock shape**: `swarm:run_job` keeps the same
  signature + same `JobOutcome` return shape. Job event channel
  unchanged. UI hooks see no diff.
- **Identifier strategy / timestamp invariant**: N/A.
- **`--no-verify` rule**: every commit in W5-06 passes the full
  verification suite. No skipped hooks.

## Scope

### Phase 1 — Test categorization

The sub-agent reads every test in `src-tauri/src/swarm/`,
`src-tauri/src/commands/swarm.rs`, and the `integration_*`
real-claude tests. Categorise each into one of three buckets:

| Bucket | Action | Estimated count |
|---|---|---|
| Pure FSM internals | DELETE | ~80 |
| Job lifecycle (FSM-driven) | REWRITE against brain | ~120 |
| Persistence / parsers / parsers-of-parsers | KEEP | ~235 |
| Real-claude integration | REWRITE against brain | ~12 |

**Pure FSM internals** examples (delete):
- `select_chain_pairs_resolves_backend_pair`
- `aggregate_rejections_synthesises_verdict`
- `next_state_from_scout_returns_classify`
- `fsm_run_loop_*` happy-path-via-mock
- `select_chain_ids_*`
- `fsm_*_falls_back_to_*`

**Job lifecycle** examples (rewrite):
- `fsm_happy_path_emits_finished` →
  `brain_happy_path_emits_finished`
- `fsm_emits_cancelled_then_finished_on_signal_cancel` →
  `brain_emits_cancelled_then_finished_on_job_cancel`
- `fsm_persists_stage_results_on_each_transition` →
  `brain_persists_stage_results_via_projector`
- `fsm_workspace_busy_when_concurrent` →
  `brain_workspace_busy_when_concurrent` (uses W5-05 guard)

**Keep** examples:
- `parse_verdict_*` (W3-12d)
- `parse_decision_*` (W3-12f)
- `parse_help_request_*` (W4-05)
- `parse_orchestrator_outcome_*` (W3-12k1)
- `parse_brain_action_*` (W5-03)
- `swarm_jobs_table_round_trip`
- `swarm_stages_*`
- All `mailbox_*` from W5-01
- All projector tests from W5-04

**Real-claude integration** (rewrite):
- `integration_research_only_real_claude` →
  `integration_research_only_real_claude_brain`
- `integration_full_chain_real_claude_with_verdict` →
  `integration_full_chain_real_claude_brain_with_verdict`
- `integration_fsm_drives_real_claude_chain` → DELETE (FSM-
  specific)
- `integration_persistent_two_turn_real_claude` → KEEP (transport-
  level, not FSM)
- `integration_fullstack_parallel_chain_real_claude` →
  `integration_fullstack_chain_real_claude_brain` (sequential or
  parallel decided by the brain at runtime; no parallel
  guarantee)

### Phase 2 — Test rewrites land

Run cargo test after each batch of rewrites; ensure the test
count is monotonic-increasing toward the W5-06 target. The
rewrites land alongside the FSM (the FSM is still alive at this
point — its tests get replaced one-by-one).

Acceptance gate at end of Phase 2:
- `cargo test --lib` exits 0
- New brain-driven lifecycle tests cover every assertion the
  deleted FSM lifecycle tests covered

### Phase 3 — `swarm:run_job` migration

Rename `swarm:run_job_v2` → `swarm:run_job` in
`src-tauri/src/commands/swarm.rs`:
- Delete the existing `swarm_run_job` function (the FSM driver).
- Rename `swarm_run_job_v2` → `swarm_run_job`.
- Update specta `collect_commands!` in `lib.rs`: remove the v2
  entry, keep only the (now brain-driven) `swarm_run_job`.
- `bindings.ts` regen: the v2 command disappears; the v1
  command's signature is unchanged from the frontend's
  perspective (same input, same output shape).

The Orchestrator chat panel (`OrchestratorChatPanel.tsx`) calls
`commands.swarmRunJob` already — no frontend code change.

### Phase 4 — FSM module deletion

Delete `src-tauri/src/swarm/coordinator/fsm.rs` (7322 lines).
Move:
- `SCOUT_PROMPT_TEMPLATE` / `PLAN_PROMPT_TEMPLATE` /
  `BUILD_PROMPT_TEMPLATE` / `CLASSIFY_PROMPT_TEMPLATE` →
  `src-tauri/src/swarm/prompts.rs` (new module). The brain's
  persona is responsible for the prompt-rendering decision now,
  but the templates may still be useful as defaults the brain
  references via the persona body.
- `MAX_RETRIES` constant → either to `prompts.rs` as a hint
  the brain persona references, or deleted (the brain decides
  retry budget LLM-side).
- `select_chain_pairs` / `BuilderDomain` / `builder_domain_for`
  → DELETE (brain replaces this logic).

Update `src-tauri/src/swarm/coordinator/mod.rs`:
- Remove `pub mod fsm;` and the `pub use fsm::*` line.
- Remove `CoordinatorFsm` from `pub use`.

Update `src-tauri/src/swarm/mod.rs`:
- Remove `CoordinatorFsm` from re-exports.

Update `src-tauri/src/lib.rs`:
- Remove the FSM-specific setup code. The brain's setup (W5-03)
  takes its place.
- The `JobRegistry` (W3-12b) might be retained if the projector
  (W5-04) reuses it for SQL writes; otherwise delete the
  workspace-locks portion.

### Phase 5 — Final integration battery

Run the rewritten real-claude integration tests sequentially:

```powershell
$env:NEURON_SWARM_STAGE_TIMEOUT_SEC="600"
$env:NEURON_BRAIN_MAX_DISPATCHES="20"
cargo test --lib integration_research_only_real_claude_brain -- --ignored --nocapture --test-threads=1
cargo test --lib integration_full_chain_real_claude_brain_with_verdict -- --ignored --nocapture --test-threads=1
cargo test --lib integration_fullstack_chain_real_claude_brain -- --ignored --nocapture --test-threads=1
cargo test --lib integration_persistence_survives_real_claude_chain_brain -- --ignored --nocapture --test-threads=1
```

Each test:
- Same goal text the W3-12 smokes use
- Asserts `JobOutcome.final_state == JobState::Done`
- Wall-clock budget 600s per test (some fullstack jobs may need
  more — bump if needed, document in AGENT_LOG)

LLM-flaky behavior (one of: a builder misinterprets the goal as
"verification", a reviewer rejects on a borderline case, the
brain hits the max-dispatches cap before terminating) is
expected. The acceptance gate is "passes within 3 retries on a
fresh subprocess pool", consistent with W3-12i / W3-12j caveats.

### Phase 6 — AGENT_LOG retrospective

Append entry covering:
- Final test count delta (435 → ~350 expected; document actual)
- Deleted LOC (7322+ lines from fsm.rs alone)
- Brain dispatcher avg dispatch count per goal vs FSM stage count
  (e.g. brain may take 8 dispatches where FSM took 6)
- Wall-clock comparison: brain vs FSM on the same goal
- Real-claude smoke pass rate

## Out of scope

- ❌ Multi-workspace UX (still single-workspace per Charter §9)
- ❌ Specialist-to-specialist direct comms (still Coordinator-
  mediated per W4 decision 3C)
- ❌ Cross-app-restart session persistence (post-W5)
- ❌ Streaming brain decisions (one-shot per turn)
- ❌ Removing the W3-12k1 Orchestrator chat brain — that lives
  ABOVE the dispatcher and stays. Only the FSM (the layer below
  Orchestrator) is removed.
- ❌ Removing W4-05 help-loop substrate — `help_request.rs`
  parsers are still used by the W5-03 brain; only the
  transparent-help-loop wrapper around `acquire_and_invoke_turn`
  in `RegistryTransport` is removed (since `RegistryTransport`
  is a Transport for the FSM, and the FSM is gone). The brain
  uses `acquire_and_invoke_turn` directly + the bus-mediated
  help-loop.

## Acceptance criteria

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` exits 0; total ≥ 350 (down from 435)
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regen + commit
- [ ] `pnpm gen:bindings:check` exits 0
- [ ] `pnpm typecheck` / `pnpm lint` / `pnpm test --run` exit 0
- [ ] All four rewritten real-claude smokes pass within 3 retries
- [ ] `swarm/coordinator/fsm.rs` does not exist on the filesystem
- [ ] `grep -r "CoordinatorFsm" src-tauri/` returns zero matches
- [ ] AGENT_LOG.md retrospective entry appended

## Verification commands

```powershell
cd src-tauri
cargo build --lib
cargo test --lib
cargo check --all-targets

# Ensure FSM gone
if (Test-Path "src/swarm/coordinator/fsm.rs") { throw "fsm.rs still exists" }
if ((Select-String -Path "src/**/*.rs" -Pattern "CoordinatorFsm" -List).Count -gt 0) { throw "CoordinatorFsm still referenced" }

# Real-claude smokes (60-90 minutes total)
$env:NEURON_SWARM_STAGE_TIMEOUT_SEC="600"
cargo test --lib _real_claude_brain -- --ignored --nocapture --test-threads=1
cd ..

pnpm gen:bindings
git add app/src/lib/bindings.ts
pnpm gen:bindings:check
pnpm typecheck
pnpm lint
pnpm test --run
```

## Files allowed to modify

- `src-tauri/src/swarm/coordinator/fsm.rs` (DELETE)
- `src-tauri/src/swarm/coordinator/mod.rs` (remove FSM
  re-exports)
- `src-tauri/src/swarm/mod.rs` (remove FSM re-export)
- `src-tauri/src/swarm/prompts.rs` (NEW — receive prompt
  templates + retry hint)
- `src-tauri/src/commands/swarm.rs` (`swarm_run_job_v2` →
  `swarm_run_job`; delete old `swarm_run_job`)
- `src-tauri/src/lib.rs` (specta + collect_commands cleanup)
- `src-tauri/src/swarm/agent_registry.rs` (remove `RegistryTransport`
  if no longer used; the brain calls
  `acquire_and_invoke_turn` directly without a Transport
  abstraction)
- `src-tauri/src/swarm/transport.rs` (one-shot
  `SubprocessTransport` may still be used by `swarm:test_invoke`
  IPC — keep or delete based on use)
- `src-tauri/src/swarm/help_request.rs` (`process_help_request`
  registry-level helper deleted; parsers stay)
- All test files under `src-tauri/src/swarm/` and
  `src-tauri/src/commands/swarm.rs` (rewrite per Phase 1
  categorisation)
- `app/src/lib/bindings.ts` (regen)
- `docs/work-packages/WP-W5-06-fsm-deprecation.md`
- `AGENT_LOG.md`

MUST NOT touch:
- W5-01..05 modules' core surface (mailbox_bus, projector,
  agent_dispatcher, brain) — only the wiring
- W3-12k1 Orchestrator brain (`orchestrator.rs` /
  `orchestrator_session.rs`) — stays
- Verdict / Decision / HelpRequest parsers — stay
- Persona files (the brain uses the existing `coordinator.md`
  with the W5-03 dispatch protocol section)
- Frontend components (no UI change)

## Split escape hatch

If the sub-agent kickoff blows past target time on Phase 2 (test
rewrites, the largest segment), split:

- **W5-06a**: Phase 1 (categorise) + Phase 2 (rewrite tests).
  FSM stays alive. ~120 test rewrites + ~80 deletions. Acceptance
  gate: `cargo test --lib` exits 0 with the new brain-driven
  lifecycle tests passing.
- **W5-06b**: Phases 3-6 (rename IPC, delete FSM module, run
  real-claude smokes). Smaller scope; mostly file deletion +
  IPC rename.

The split point is the post-Phase-2 cargo green gate.

## Notes / risks

- **LOC delta** is dominated by the 7322-line fsm.rs. Other
  small deletions: `select_chain_pairs` helper, `BuilderDomain`,
  `builder_domain_for`, the `'retry_loop` block. Net deletion
  ~7500 lines. The brain (W5-03) added ~1000 lines + projector
  (W5-04) ~600 + dispatcher (W5-02) ~500 + bus (W5-01) ~700.
  Net W5 LOC: roughly -4700. Smaller, simpler swarm runtime.

- **Brain wall-clock vs FSM**: brain may take more dispatches
  than FSM's hardcoded chain (e.g. brain dispatches scout twice
  by mistake, or asks Coordinator a clarifying question that
  the FSM didn't need). Mitigation: the persona body's hard
  contract constraints + max-dispatches cap. If a smoke
  consistently goes over budget, document in AGENT_LOG and
  consider tightening the persona body.

- **Reviewer-rejection retry under brain**: today's FSM has a
  hardcoded retry loop (`MAX_RETRIES=2` after a Verdict.rejected).
  Brain decides retry LLM-side based on the Verdict's
  `assistant_text`. May retry more or fewer times than the FSM.
  Acceptance: brain's behavior is "reasonable" — verified by
  the real-claude smokes ending in `Done` for goals that
  previously ended in `Done` under FSM.

- **AGENT_LOG retrospective entry**: should include a comparison
  table:
  | Goal | FSM stages | Brain dispatches | FSM wall-clock | Brain wall-clock | Final state |
  Helps post-W5 readers calibrate expectations.

- **`grep -r "CoordinatorFsm"`** verification: the sub-agent
  must do this grep at the end and assert zero matches. If any
  doc file mentions `CoordinatorFsm`, it stays as historical
  reference (not a failed acceptance check) — the grep is on
  `src-tauri/src/`, not `docs/`.

- **`SubprocessTransport` retention**: today's
  `swarm:test_invoke` IPC uses one-shot `SubprocessTransport`
  for ad-hoc persona testing. Keep it (the brain doesn't use
  it, but the IPC is useful for debugging personas). The
  `RegistryTransport` adapter (W4-06) is gone since it was
  FSM-specific.

- **`JobRegistry` lifecycle**: the projector (W5-04) uses
  swarm_jobs / swarm_stages rows. The W3-12b `JobRegistry`
  in-memory + workspace-lock state may not be needed once the
  FSM is gone. W5-06 evaluates and either deletes the registry
  entirely or trims it to a thin SQL-backed query helper.
  Default: trim — workspace-lock duties move to W5-05's
  bus-level guard.

## Result

(Filled in by the sub-agent on completion. Sections:
`Test count delta`, `Deleted LOC`, `Real-claude smoke results`,
`Brain vs FSM dispatch comparison`, `Caveats`.)

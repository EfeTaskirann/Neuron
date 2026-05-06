---
id: WP-W3-12i
title: Coordinator FSM — Fullstack sequential dispatch (BB+BR then FB+FR)
owner: TBD
status: not-started
depends-on: [WP-W3-12h]
acceptance-gate: "scope=Fullstack runs the full 8-stage chain: Scout → Classify → Plan → BuildBackend → ReviewBackend (gate) → BuildFrontend → ReviewFrontend (gate) → Test (gate) → Done. `Job.stages` has 8 entries with correct specialist_ids per row. Plan prompt template carries scope so Planner produces a dual-domain plan. Retry loop re-runs the whole chain on any gate rejection (per-domain retry is a future polish)."
---

## Goal

Activate the architectural report §2.1's full multi-domain
specialist team for Fullstack goals. Today (post-W3-12h)
scope=Fullstack falls back to backend chain with a `tracing::warn!`.
This WP makes Fullstack actually run BB+BR then FB+FR sequentially.

W3-12j (future) parallelizes the two domains via `tokio::join!`.
12i is sequential to keep the FSM mental model linear and the
test surface tractable.

## Why now

The 9-agent vision (per architectural report §2.1 + owner
directive 2026-05-06 "9 agent ekibi bu yüzden istiyorum") needs
a goal type that exercises ALL 8 active specialists in one job.
Today no goal can do that — Backend goals run BB+BR, Frontend
goals run FB+FR, but no goal runs both. Fullstack sequential is
the bridge.

Practical use case: "Add a `/me` Tauri IPC command in
src-tauri/src/commands/me.rs AND wire it into a useMe hook in
app/src/hooks/useMe.ts." That's a textbook fullstack edit; no
single domain reviewer can sensibly approve both halves.

## Charter alignment

No tech-stack change. FSM-internal sequential restructure.

## Scope

### 1. New helper `select_chain_pairs(scope)`

Replaces W3-12h's `select_chain_ids(scope) -> (&'static str, &'static str)` with:

```rust
fn select_chain_pairs(scope: CoordinatorScope)
    -> Vec<(&'static str, &'static str)> {
    match scope {
        CoordinatorScope::Backend => vec![
            (BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID),
        ],
        CoordinatorScope::Frontend => vec![
            (FRONTEND_BUILDER_ID, FRONTEND_REVIEWER_ID),
        ],
        CoordinatorScope::Fullstack => vec![
            (BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID),
            (FRONTEND_BUILDER_ID, FRONTEND_REVIEWER_ID),
        ],
    }
}
```

Existing `select_chain_ids` is removed (no callers post-12i)
OR kept as a thin wrapper that picks the first pair (backwards
compat for any downstream code; not strictly necessary because
it's `pub(crate)`).

### 2. Run loop iterates over pairs

The W3-12e retry loop body currently does:
```
PLAN
  let (builder_id, reviewer_id) = select_chain_ids(scope);
  let builder = profiles.get(builder_id);
  let reviewer = profiles.get(reviewer_id);
  BUILD
  REVIEW (gate — rejection triggers retry-or-fail)
TEST (gate)
break Done
```

Becomes:
```
PLAN
  let pairs = select_chain_pairs(scope);
  for (builder_id, reviewer_id) in pairs {
    let builder = profiles.get(builder_id);
    let reviewer = profiles.get(reviewer_id);
    BUILD
    REVIEW (gate — rejection triggers retry-or-fail; continue on approve)
  }
TEST (gate)
break Done
```

For Backend/Frontend (single pair) the for-loop runs once —
identical behavior to W3-12h. For Fullstack (two pairs) it runs
twice sequentially.

Rejection inside the loop:
- BR rejects at attempt 1 → `try_start_retry` fires; `continue 'retry_loop` re-runs Plan + the WHOLE pairs sequence.
- This means a fullstack job whose backend approved but frontend rejected re-runs backend stages on retry. Wasteful but correct. **Per-domain retry is a future polish** (W3-12i+ or W3-12m).

### 3. Plan prompt template gains `scope` field

`PLAN_PROMPT_TEMPLATE` becomes:

```rust
const PLAN_PROMPT_TEMPLATE: &str = "Hedef: {goal}\n\
\n\
Kapsam: {scope}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
Bu hedef için adım adım bir plan üret. Kapsam fullstack ise \
plan hem backend (Rust/SQL) hem frontend (TS/React/CSS) \
adımlarını içermelidir.\n";
```

`render_plan_prompt(goal, scout_output, scope)` adds the scope
parameter. Helper formats scope to "backend" / "frontend" /
"fullstack".

`RETRY_PLAN_PROMPT_TEMPLATE` similarly gains scope field. The
retry helper `render_retry_plan_prompt` already takes `verdict`
and `gate`; adds `scope` parameter.

### 4. Persistence — no schema change

`Job.stages` already supports an unbounded vec of `StageResult`.
For Fullstack, stages.len() can be 8 on the happy path or more
(with retries: 8 × N attempts). Existing `swarm_stages` table
schema accommodates this — `(job_id, idx)` composite PK with
idx growing monotonically.

`store::insert_stage` and `store::get_job_detail` already work
across N stages. No SQL change.

### 5. Tests

#### Pure-fn tests
- `select_chain_pairs_backend_returns_one_pair`.
- `select_chain_pairs_frontend_returns_one_pair`.
- `select_chain_pairs_fullstack_returns_two_pairs` — assert order: backend first, then frontend.

#### FSM tests (mock-driven)

- `fsm_scope_fullstack_walks_eight_stages_on_approved_path` — mock all 4 specialists (Scout, Classify, Plan, BB, BR, FB, FR, Test) returning Ok + approved Verdicts; assert `outcome.final_state == Done`, `outcome.stages.len() == 8`, stage IDs in order: scout, coordinator, planner, backend-builder, backend-reviewer, frontend-builder, frontend-reviewer, integration-tester.
- `fsm_scope_fullstack_backend_review_rejection_retries_full_chain` — mock BR rejects attempt 1, approves attempt 2; FR + Test approve. Assert `final_state == Done`, `retry_count == 1`, total stages == 8 (attempt 1: 5 stages — scout/classify/plan/BB/BR-rejected → finalize_failed_with_verdict NOT called because retry kicks in) + 8 (attempt 2: full happy path) → wait, actually with the current FSM's retry, the failed BR attempt's stages aren't pushed (per W3-12d "stages.is_empty on scout failure" pattern, but verdict-rejected stages ARE pushed because that's `run_verdict_stage` outcome). Let me state the actual count: attempt 1 = scout(1) + classify(1) + plan(1) + BB(1) + BR-rejected(1) = 5 in stages; attempt 2 = plan(1) + BB(1) + BR-approved(1) + FB(1) + FR-approved(1) + test(1) = 6 in stages; total = 11. (Scout cached, doesn't re-run.) Assert this exact count.
- `fsm_scope_fullstack_frontend_review_rejection_retries_full_chain` — analogous but FR rejects.
- `fsm_scope_fullstack_test_rejection_retries_full_chain` — analogous but Test rejects.
- `fsm_scope_fullstack_exhausts_retries_finalizes_failed` — mock FR rejects on every attempt; after MAX_RETRIES=2, finalizes Failed with `last_verdict.approved == false`.
- `plan_prompt_includes_fullstack_scope_when_scope_is_fullstack`.
- `plan_prompt_includes_backend_scope_when_scope_is_backend`.
- `plan_prompt_includes_frontend_scope_when_scope_is_frontend`.
- `retry_plan_prompt_carries_scope` — analogous for the retry variant.
- `fsm_fullstack_persistence_round_trip` — drive a fullstack happy path through the with-pool registry, reload via `store::get_job_detail`, assert all 8 stages and per-stage specialist_ids round-trip.

#### Existing FSM regression

The existing scope=Backend and scope=Frontend tests must still pass — the for-loop runs once for them, identical behavior. No bulk update.

### 6. Integration test (`#[ignore]`d)

`integration_fullstack_chain_real_claude` — real-claude smoke for the new Fullstack flow:

```rust
let goal = "Add a one-line doc comment to TWO files: \
    (1) above the `Job` struct definition in \
    src-tauri/src/swarm/coordinator/job.rs, briefly noting that \
    Job carries the full lifecycle of a swarm run; \
    (2) above the `formatRelativeMs` function exported from \
    app/src/components/SwarmJobList.tsx, briefly noting that \
    the helper rounds to the nearest minute. \
    Just the two doc comments. Do not change behavior.";
```

This is a textbook fullstack edit: one Rust file, one TSX file,
both doc-only.

Expectations:
- `outcome.final_state == Done`
- `outcome.stages.len() == 8`
- Stage IDs in order: scout, coordinator, planner, backend-builder, backend-reviewer, frontend-builder, frontend-reviewer, integration-tester
- Coordinator's decision.scope == Fullstack

Time budget: 8 stages × 180s/stage = 24 min worst-case. Typical
6-10 min once warm. The real-claude smoke is owner-driven AND
this orchestrator's.

### 7. Bindings

NO wire-shape changes. `select_chain_pairs` is internal. `Job.stages`
shape is unchanged.

`pnpm gen:bindings:check` exits 0 post-commit.

### 8. UI follow-up note

`SwarmJobDetail.tsx` already renders per-stage `specialist_id`.
Fullstack jobs will show 8 stage rows (vs 6 for single-domain).
The retry-counter pill from W3-14-followup still works — UI
needs no change.

## Out of scope

- ❌ Fullstack parallel dispatch via `tokio::join!`. W3-12j.
- ❌ Per-domain retry budget (BR rejection re-runs only Backend stages; FR rejection re-runs only Frontend). Currently retries re-Plan everything. Future polish.
- ❌ Per-domain Plan output (one plan covers both domains today; could split into "backend plan" + "frontend plan" sub-sections but Planner persona handles this naturally with the scope hint).
- ❌ Cross-domain Verdict (e.g. "BackendReviewer notes a backend issue caused by a frontend assumption"). Independent gates.
- ❌ Scope override at job level.
- ❌ UI scope pill in SwarmJobDetail.tsx header.

## Acceptance criteria

- [ ] `select_chain_pairs(scope) -> Vec<(&'static str, &'static str)>` helper added.
- [ ] FSM run loop iterates over `select_chain_pairs(decision.scope)` for Build/Review.
- [ ] PLAN_PROMPT_TEMPLATE and RETRY_PLAN_PROMPT_TEMPLATE include `{scope}` field.
- [ ] `render_plan_prompt(goal, scout_output, scope)` and `render_retry_plan_prompt(...scope)` updated.
- [ ] `tracing::warn!` block from W3-12h removed (Fullstack now correctly dispatches; no longer falls back).
- [ ] `tracing::info!` line covering route + scope still fires (visibility audit).
- [ ] Mock FSM tests for Fullstack happy path + 3 rejection paths + retry-exhaust pass.
- [ ] Existing W3-12d/e/f/g/h tests pass without modification (Backend/Frontend flows unchanged).
- [ ] Integration test (`#[ignore]`d) `integration_fullstack_chain_real_claude` compiles.
- [ ] No new dep, no new migration, no `unsafe`, no `eprintln!`.
- [ ] All Week-2 + Week-3-prior tests pass; target ≥340 passing.
- [ ] `bindings.ts` unchanged (verified by `pnpm gen:bindings:check` exit 0).

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # SHOULD exit 0 — no wire shape changes
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration smokes:
cd src-tauri
cargo test --lib -- integration_fullstack_chain_real_claude --ignored --nocapture
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
cargo test --lib -- integration_frontend_chain_real_claude --ignored --nocapture
```

## Notes / risks

- **Sequential is wasteful on retry.** A fullstack job whose backend approved but frontend rejected re-runs backend stages on retry. ~$0.10-0.20 extra per retry cycle. Per-domain retry is a future polish; quality gain (correct full chain) outweighs cost per owner directive.
- **8-stage wall clock.** Real-claude fullstack happy path ≈ 5-10 min. Cancel during stages 5-7 (FB/FR/Test) is the most likely user behavior — already supported via W3-12c's per-stage `tokio::select!`.
- **Plan prompt for Fullstack.** Planner sees scope=fullstack and produces a plan covering both domains. The "step 1 only" instruction in BUILD_PROMPT_TEMPLATE means BackendBuilder might pick step 1 = backend step (good), then later FrontendBuilder picks step 1 of the same plan and might re-implement the backend step. **Mitigation**: the BuildBackend stage's actual output (assistantText) is what the FrontendBuilder sees in the loop's continuation — but the prompt for FrontendBuilder is `render_build_prompt(plan_text)` which is the SAME plan. FrontendBuilder needs to know to skip the backend steps. Fix: extend `render_build_prompt` to take `domain` (Backend/Frontend) and inject "Sen sadece bu domain'e ait adımları uygula" rule.
  - **Build prompt scope-aware**: `render_build_prompt(plan_text, builder_domain: Domain)` produces "Aşağıdaki Plan'ın 1. adımını uygula" for Backend, "Aşağıdaki Plan'da frontend tarafına dair olan ilk adımı uygula" for Frontend. The exact wording is the persona's job; the FSM only varies a single sentence.
  - This is a meaningful prompt-engineering nuance. Sub-agent should think about this carefully when implementing the Fullstack run loop.
- **Stage idx growth on retries.** Test count assertions need to account for the rejected-stage-also-counts pattern (verdict-rejected stages are pushed; stage-error stages are not). Documentation in tests.
- **Test asserts exact stage IDs in order.** This couples tests to the FSM ordering. Acceptable — the contract IS the ordering, and changing it would require explicit test updates anyway.
- **Cancel during FrontendBuilder.** Tests for cancel mid-fullstack-job — `cancelled_during` field will report `JobState::Build` regardless of which builder was running. Future polish could track which-builder-was-running but JobState is stage-level not specialist-level.

## Sub-agent reminders

- Read this WP in full.
- Read `src-tauri/src/swarm/coordinator/fsm.rs` (W3-12h state) — particularly the `select_chain_ids` helper and the run loop's Builder/Reviewer profile resolution.
- Read `src-tauri/src/swarm/agents/planner.md` to understand the Planner persona — it's already domain-agnostic; the scope hint in the prompt template is enough to nudge it toward fullstack plans.
- DO NOT add a new dep.
- DO NOT add Fullstack parallel dispatch. That's W3-12j.
- DO NOT change retry-loop semantics (re-Plan from scratch on rejection). Per-domain retry is a future WP.
- DO NOT change the existing scope=Backend or scope=Frontend behavior. The for-loop runs once for them.
- DO NOT add a new SwarmJobEvent variant. The existing StageStarted/StageCompleted carry enough info (specialist_id distinguishes domains).
- DO consider the "Build prompt scope-aware" risk in §"Notes / risks": the Builder needs to know which domain's step to pick from a Fullstack plan. Implement `render_build_prompt(plan_text, builder_domain)` if the simpler "all builders see the same plan" approach causes test failures or persona confusion.
- Per AGENTS.md: one WP = one commit.

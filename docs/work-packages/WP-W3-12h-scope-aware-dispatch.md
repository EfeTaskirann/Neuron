---
id: WP-W3-12h
title: Coordinator FSM — scope-aware single-domain dispatch (Backend / Frontend)
owner: TBD
status: not-started
depends-on: [WP-W3-12g]
acceptance-gate: "FSM picks Builder + Reviewer based on `CoordinatorDecision.scope`. scope=Backend → backend-builder + backend-reviewer (current behavior). scope=Frontend → frontend-builder + frontend-reviewer (NEW activation). scope=Fullstack continues to fall back to backend chain with the existing W3-12g warning (Fullstack sequential dispatch is W3-12i). Persisted: each StageResult correctly reports its specialist_id (e.g. `frontend-builder` not `backend-builder` for frontend goals)."
---

## Goal

Activate the FrontendBuilder + FrontendReviewer profiles W3-12g
shipped. Today their personas are bundled but the FSM never
picks them — every job runs through backend-builder +
backend-reviewer regardless of Coordinator's scope output.

This WP ships **scope-aware single-domain dispatch**:
- scope=Backend → unchanged (backend chain)
- scope=Frontend → NEW (frontend chain)
- scope=Fullstack → unchanged from W3-12g (backend fallback + warn)

W3-12i activates Fullstack sequential dispatch (BB+BR then FB+FR).
W3-12j (if needed) activates Fullstack parallel dispatch
(tokio::join!). Splitting these keeps each WP M-sized and
isolates the test surface.

## Why now

12g shipped 8 profiles + scope classification. Today, a
frontend-targeted goal like "Add a doc-comment to
SwarmJobDetail.tsx explaining the cancel button visibility
rule" gets:
- Coordinator correctly classifies scope=frontend
- FSM ignores the scope, dispatches backend-builder
- backend-builder writes Rust idioms into a TSX file (or
  refuses, or struggles)
- backend-reviewer reviews TSX with Rust expectations

That's actively wrong. Single-domain scope-aware dispatch is
the smallest fix that makes 8-profile bundle DO something.

## Charter alignment

No tech-stack change. FSM-internal const selection only.

## Scope

### 1. New const + helper for scope-aware ID selection

In `src-tauri/src/swarm/coordinator/fsm.rs`:

```rust
pub const FRONTEND_BUILDER_ID: &str = "frontend-builder";
pub const FRONTEND_REVIEWER_ID: &str = "frontend-reviewer";

/// Resolve the specialist IDs for a given scope. Returns
/// (builder_id, reviewer_id). Fullstack falls back to backend
/// chain in W3-12h with a tracing::warn! at the call site;
/// W3-12i activates Fullstack sequential dispatch.
fn select_chain_ids(scope: CoordinatorScope) -> (&'static str, &'static str) {
    match scope {
        CoordinatorScope::Backend => (BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID),
        CoordinatorScope::Frontend => (FRONTEND_BUILDER_ID, FRONTEND_REVIEWER_ID),
        CoordinatorScope::Fullstack => {
            // 12h fallback: same as 12g. 12i activates the real
            // sequential / parallel dispatch.
            (BACKEND_BUILDER_ID, BACKEND_REVIEWER_ID)
        }
    }
}
```

### 2. FSM run loop — replace hardcoded IDs with helper

In `run_job_inner`, inside the `'retry_loop:` body, replace:

```rust
// W3-12g (current):
let builder = profiles.get(BACKEND_BUILDER_ID)?;
let reviewer = profiles.get(BACKEND_REVIEWER_ID)?;
```

with:

```rust
// W3-12h:
let (builder_id, reviewer_id) = select_chain_ids(decision.scope);
let builder = profiles.get(builder_id).ok_or_else(|| ...)?;
let reviewer = profiles.get(reviewer_id).ok_or_else(|| ...)?;
```

`decision` is the parsed CoordinatorDecision from the Classify
stage; it's already in scope after W3-12f's run-loop addition.

The existing `tracing::warn!` block from W3-12g (when
scope=Frontend|Fullstack) gets refined:
- For scope=Frontend: drop the warn entirely (no longer
  routing-mismatch, the chain matches the scope).
- For scope=Fullstack: keep the warn but update the message:
  "scope=fullstack detected; W3-12h still falls back to backend
  chain — W3-12i activates Fullstack sequential dispatch."

### 3. IntegrationTester behavior

The existing IntegrationTester profile already detects project
type via manifest sniffing:
- Cargo.toml → cargo test
- package.json → pnpm test

For scope=Frontend, the FSM passes the same Builder + Reviewer
output to Tester. Tester sees package.json (or both) at the
project root and runs `pnpm test`. The persona body already
handles this; no profile change needed.

If a frontend-only edit somehow doesn't trigger pnpm test (e.g.
the user goal didn't touch testable surface), Tester emits an
approved Verdict with summary noting "no relevant tests to
run" — Verdict.approved=true, issues=[]. This is an existing
edge case from W3-12d.

### 4. Tests

#### FSM tests (mock-driven)

- `select_chain_ids_backend_returns_backend_pair` — pure-fn unit test.
- `select_chain_ids_frontend_returns_frontend_pair`.
- `select_chain_ids_fullstack_falls_back_to_backend` — documents the W3-12h contract; W3-12i changes this test.
- `fsm_scope_frontend_dispatches_frontend_chain` — mock Coordinator returns scope=Frontend; assert stages[3].specialist_id == "frontend-builder" and stages[4].specialist_id == "frontend-reviewer". (stages[0]=Scout, stages[1]=Classify, stages[2]=Plan, stages[3]=Build, stages[4]=Review, stages[5]=Test.)
- `fsm_scope_backend_dispatches_backend_chain` — regression: mock scope=Backend; stages[3]=backend-builder, stages[4]=backend-reviewer.
- `fsm_scope_fullstack_falls_back_to_backend_chain_with_warn` — mock scope=Fullstack; stages[3]=backend-builder, stages[4]=backend-reviewer (current 12g behavior).
- `fsm_frontend_chain_persists_correct_specialist_ids` — write a frontend-scope job through the registry, reload via `store::get_job_detail`, assert specialist_ids round-trip correctly.
- `fsm_frontend_retry_loop_preserves_chain_choice` — mock frontend Reviewer rejecting, then approving on retry; assert retry attempt 2 ALSO uses frontend-builder + frontend-reviewer (not accidentally falling back to backend).

#### Existing FSM regression

The 30+ mock-driven FSM tests from W3-12d/e/f/g should still pass — they all mock scope=Backend (the default in `execute_plan_decision_response()` helper from W3-12g). No bulk update needed; the test fixture's helper returns scope=Backend, FSM dispatches backend chain.

#### Integration test (`#[ignore]`)

Add a NEW integration smoke for scope=Frontend:

```rust
#[tokio::test]
#[ignore = "requires real `claude` binary + Pro/Max subscription"]
async fn integration_frontend_chain_real_claude() {
    // Same setup as integration_full_chain_real_claude_with_verdict,
    // but with a frontend-targeted goal. Coordinator should
    // classify scope=frontend; FSM should dispatch
    // frontend-builder + frontend-reviewer.
    // Goal: minimal, low-risk frontend edit.
    // Time budget: 5 stages × 180s = 900s worst-case.
    let goal = "Add a one-line JSDoc comment above the \
        `formatRelativeMs` helper in app/src/components/SwarmJobList.tsx \
        explaining that the helper rounds to the nearest minute. \
        Just the doc comment. Do not add tests.";
    // ... assert outcome.final_state == Done
    // ... assert outcome.stages[3].specialist_id == "frontend-builder"
    // ... assert outcome.stages[4].specialist_id == "frontend-reviewer"
}
```

The doc-comment goal is intentional: minimal-risk edit that exercises Read+Edit on a TSX file without provoking IntegrationTester's `pnpm test` (which would build the entire frontend bundle).

### 5. Bindings regen

NO new types or fields. The 8-profile bundle and CoordinatorScope enum from W3-12g are the wire-shape baseline.

`pnpm gen:bindings:check` should already exit 0 post-12g; this WP shouldn't dirty bindings.ts.

### 6. UI follow-up note

W3-14's `SwarmJobDetail.tsx` already renders `stage.specialist_id` per stage row. Once W3-12h activates frontend chain, frontend-targeted jobs will show "frontend-builder" and "frontend-reviewer" labels in the stage rows — automatic. No UI change needed.

A future polish commit could add a scope pill ("backend" / "frontend" / "fullstack" badge) to SwarmJobDetail's header, but that's deferred per user directive 2026-05-06 ("geliştirilmesi gereken birçok noktası var bunlara sonra değinilecek").

## Out of scope

- ❌ Fullstack sequential dispatch (BB+BR then FB+FR). W3-12i.
- ❌ Fullstack parallel dispatch via `tokio::join!`. W3-12j.
- ❌ Per-domain retry budget (Backend rejection re-runs only Backend; Frontend rejection re-runs only Frontend). Currently retries re-Plan everything; per-domain retries are a future polish.
- ❌ Scope override at job level ("force frontend chain on this run"). Post-W3.
- ❌ Scope pill rendering in SwarmJobDetail.tsx header.
- ❌ Per-domain Plan template (Planner today produces one plan; W3-12i may split per-scope plans for Fullstack).

## Acceptance criteria

- [ ] `FRONTEND_BUILDER_ID` + `FRONTEND_REVIEWER_ID` consts added to `swarm/coordinator/fsm.rs`.
- [ ] `select_chain_ids(scope)` helper returns the correct pair for Backend / Frontend / Fullstack.
- [ ] FSM run loop uses `select_chain_ids(decision.scope)` instead of hardcoded backend constants.
- [ ] `tracing::warn!` from W3-12g updated: drops for scope=Frontend (no longer mismatched), keeps for scope=Fullstack with revised message.
- [ ] Mock FSM tests for scope=Frontend dispatch pass.
- [ ] `fsm_scope_frontend_dispatches_frontend_chain` confirms stages[3]/stages[4] are frontend specialists.
- [ ] Retry loop on frontend rejection preserves frontend chain (verified by `fsm_frontend_retry_loop_preserves_chain_choice`).
- [ ] All Week-2 + Week-3-prior tests still pass; target ≥330 passing.
- [ ] No new dep, no new migration, no `unsafe`, no `eprintln!`.
- [ ] `bindings.ts` unchanged (verified by `pnpm gen:bindings:check` exit 0).
- [ ] Integration test `integration_frontend_chain_real_claude` compiles (`#[ignore]`d).

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
cargo test --lib -- integration_frontend_chain_real_claude --ignored --nocapture
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
```

## Notes / risks

- **Frontend Test stage with no test changes.** If the goal is doc-only ("add a JSDoc comment"), `pnpm test` likely passes trivially (no test files modified). The Tester persona emits approved Verdict. This is the same pattern as the W3-12d backend integration test.
- **Workspace overrides for the renamed `reviewer` ID.** If a user has `<app_data_dir>/agents/reviewer.md` from before W3-12g, that file still loads under id `reviewer` but FSM no longer references it. CHANGELOG note added in W3-12g.
- **Scope defaulting on parse failure.** W3-12g made scope optional (serde default = Backend). When Coordinator output is unparseable, fallback decision has scope=Backend → backend chain runs. Same fail-open pattern as the route field.
- **Retry-loop interaction.** W3-12e's retry re-runs from Plan. The chain selection happens at Build/Review time using the same `decision.scope`. Frontend retries re-pick frontend chain (verified by test). NOT split-domain retry; that's a future polish.
- **Cost note.** Frontend goals now cost the same as backend goals (same 6-stage chain). Quality gain: persona-appropriate review.

## Sub-agent reminders

- Read this WP in full.
- Read `src-tauri/src/swarm/coordinator/fsm.rs` (W3-12g state) for the Classify stage and decision parsing.
- Read `src-tauri/src/swarm/agents/{frontend-builder,frontend-reviewer}.md` to understand the personas the FSM is now activating.
- DO NOT add a new dep.
- DO NOT change retry-loop semantics. Per-domain retry is a future WP.
- DO NOT add Fullstack sequential or parallel dispatch. Both are future WPs.
- DO NOT change Coordinator profile body. The scope rules are 12g's contract; 12h consumes them.
- DO NOT change persona bodies for frontend-builder / frontend-reviewer. They were authored in 12g; 12h just dispatches them.
- Per AGENTS.md: one WP = one commit.

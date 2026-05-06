---
id: WP-W3-12g
title: Swarm specialist roster expansion (6 → 8 profiles; backend/frontend split)
owner: TBD
status: not-started
depends-on: [WP-W3-12f]
acceptance-gate: "`swarm:profiles_list` returns 8 bundled profiles. The previously generic `reviewer.md` is renamed to `backend-reviewer.md`; `frontend-builder.md` and `frontend-reviewer.md` are new. FSM constants and tests use the new IDs. Coordinator profile body extended to also classify a `scope` field (backend | frontend | fullstack). Wire shape: `CoordinatorDecision` gains a `scope` field. FSM behavior UNCHANGED in 12g — it always uses backend-builder / backend-reviewer regardless of scope (W3-12h activates scope-aware dispatch)."
---

## Goal

Move the bundled specialist roster from the W3-11/d/f's
6-profile state toward the architectural report §2.1's
9-agent vision. This WP delivers profiles 5-8 of 9
(skipping Orchestrator, which is W3-12i territory because
it's a user-facing chat layer, not a specialist).

This WP is intentionally **roster-only**:
- New profiles ship in the bundle
- Coordinator profile gains scope classification
- `CoordinatorDecision.scope: CoordinatorScope` lands on the
  IPC + persistence
- **FSM behavior does NOT change** — it still dispatches
  backend-builder + backend-reviewer regardless of scope. A
  warning logs when Coordinator emits scope=Frontend or
  scope=Fullstack so the upcoming W3-12h work has visible
  signal that the routing data is being produced correctly.

Splitting the roster work from the FSM scope-dispatch work
keeps each WP M-sized and lets W3-12h focus purely on the
fan-out logic (sequential vs parallel Builder/Reviewer
chains).

## Why now

The user directive 2026-05-06: "maliyet önemli değil, kodun
temizliği ve kalitesi önemli — 9 agent ekibi bu yüzden
istiyorum." Quality-first, full team.

Today's quality gate is one Reviewer + one IntegrationTester.
The architectural report §2.1 specifies separate domain
reviewers — backend (Rust + SQL semantics) vs frontend (React
+ TS + CSS + a11y). A single generic Reviewer can't reasonably
hold both surfaces in its persona; specialization keeps each
review tight and idiomatic.

For frontend-heavy goals ("rebuild the Swarm route's verdict
panel with better a11y"), a generic Reviewer that's
backend-flavored either over-approves (misses CSS/aria
issues) or rejects on irrelevant grounds. A FrontendReviewer
profile fixes that.

## Charter alignment

No tech-stack change. The 6 → 8 profile expansion is
authorial work (new `.md` files); FSM and persistence shape
are unchanged in 12g.

Charter §"Hard constraints" #4 (OKLCH only, no hex/HSL) is a
natural fit for FrontendReviewer's persona checklist.

## Scope

### 1. Rename `reviewer.md` → `backend-reviewer.md`

The existing W3-12d profile is Rust-focused in spirit (it
was tested against `cargo check` / `cargo test` outputs).
Renaming clarifies its scope and frees the `reviewer` ID
slot.

`src-tauri/src/swarm/agents/reviewer.md` → `backend-reviewer.md`.
Frontmatter `id: reviewer` → `id: backend-reviewer`.
Persona body updated to explicitly mention: "I review Rust,
SQL migrations, and Tauri command surfaces." Allowed-tools and
permission-mode unchanged (Read/Grep/Glob, plan).

Existing `REVIEWER_ID` const in `swarm/coordinator/fsm.rs`
becomes `BACKEND_REVIEWER_ID`. Find-and-replace across:
- `swarm/coordinator/fsm.rs` (const + 5+ usage sites)
- All FSM tests that hardcode `"reviewer"` as the mock-key
- Profile-loader tests (`bundled_six_profiles_present` →
  `bundled_eight_profiles_present`)
- `commands/swarm.rs` test that asserts profile count
  (`profiles_list_returns_six_bundled` →
  `profiles_list_returns_eight_bundled`)

### 2. New `frontend-reviewer.md`

`src-tauri/src/swarm/agents/frontend-reviewer.md`.

```yaml
---
id: frontend-reviewer
version: 1.0.0
role: FrontendReviewer
description: Read-only frontend code reviewer. Reviews React/TS/CSS for correctness, a11y, design-system compliance.
allowed_tools: ["Read", "Grep", "Glob"]
permission_mode: plan
max_turns: 8
---
```

Body — strict prompt engineering per architectural report §7.2:

- Persona: "Sen bir React + TypeScript + CSS frontend code reviewer'sın. Builder'ın çıktısını okuyup şu kriterler üzerinden değerlendirirsin..."
- Review checklist:
  - **Type correctness**: `any` kullanımı yok, exhaustive switch'ler `never` ile kapanıyor, tip daraltmaları doğru.
  - **React idiom**: useEffect cleanup'lar StrictMode-safe, key prop'ları doğru, controlled inputs uncontrolled'a kaymıyor.
  - **a11y**: aria-label / role / focus management, keyboard navigation desteği.
  - **Design-system compliance** (Charter §"Hard constraints" #4): yeni CSS hex/HSL değil OKLCH; yeni renkler `var(--*)` token'lara bağlı.
  - **Tanstack Query patterns**: query keys tutarlı, cache invalidation doğru zamanda, optimistik update'ler StrictMode-safe.
- OUTPUT CONTRACT: same shape as backend-reviewer (Verdict JSON).
- Few-shot examples: 1 approved (clean React component), 1 rejected (missing aria, hex color, useEffect leak).
- Negative examples: no fence, no preamble.
- "Sen Coordinator değil sen Specialist'sin" reminder.

### 3. New `frontend-builder.md`

`src-tauri/src/swarm/agents/frontend-builder.md`.

```yaml
---
id: frontend-builder
version: 1.0.0
role: FrontendBuilder
description: Implements React/TS/CSS atomic plan steps. Writes code, runs typecheck/lint, returns one-shot result.
allowed_tools: ["Read", "Edit", "Write", "Grep", "Glob", "Bash(pnpm *)", "Bash(npm test *)"]
permission_mode: acceptEdits
max_turns: 16
---
```

Body — mirrors `backend-builder.md`'s pattern but for React surface:

- Persona: senior React + TS + CSS engineer.
- Reads first (Glob the file pattern, Read the target), then writes (Edit / Write), then validates (`pnpm typecheck` and/or `pnpm test --run`).
- Scope-disciplined: applies ONE plan step per invocation.
- Charter constraints inline: OKLCH only, design-system tokens, no `any`, exhaustive matches.
- max_turns 16 (vs backend-builder's 12) because frontend changes often touch component + style + test trio in a single step.

### 4. Update `coordinator.md` to emit `scope` field

The existing W3-12f Coordinator emits `{ "route": ..., "reasoning": ... }`.

Extend OUTPUT CONTRACT:

```json
{
  "route": "research_only" | "execute_plan",
  "scope": "backend" | "frontend" | "fullstack",
  "reasoning": "..."
}
```

Decision rules added to the persona body:
- **scope=backend**: goal mentions Rust files (`.rs`), `Cargo.toml`, SQL/migrations, `src-tauri/`, `swarm/`, sidecar/agent.rs, etc.
- **scope=frontend**: goal mentions `.tsx`, `.jsx`, `.css`, `app/`, `app/src/`, "UI", "component", "route", "hook" (in TS/React sense), Tauri's frontend invoke pattern.
- **scope=fullstack**: goal mentions both, OR mentions an end-to-end feature ("add a /me endpoint AND its frontend display"), OR is unclear/cross-cutting.
- **route=research_only**: scope is informational only; FSM ignores it (Scout's findings are the deliverable). Still emit `scope=backend|frontend|fullstack` based on which surface Scout would investigate, for audit-trail clarity.

Few-shot examples updated to cover the 3 × 2 = 6 combinations
(2 are most common: backend+execute, frontend+execute).

### 5. New `CoordinatorScope` enum + `CoordinatorDecision.scope`

In `swarm/coordinator/decision.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorScope {
    Backend,
    Frontend,
    Fullstack,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct CoordinatorDecision {
    pub route: CoordinatorRoute,
    pub scope: CoordinatorScope,    // NEW
    pub reasoning: String,
}
```

`parse_decision` continues to use the same 4-step robust
parser — adding a required field changes the JSON contract but
not the parsing strategy.

**Backwards compat for existing persisted decisions**: pre-12g
rows in `swarm_stages.decision_json` lack the `scope` field.
On deserialization, missing `scope` defaults to
`CoordinatorScope::Backend` (matching the existing FSM
behavior — backend-only chain). Done via a custom serde
default attribute.

```rust
#[derive(...)]
pub struct CoordinatorDecision {
    pub route: CoordinatorRoute,
    #[serde(default = "CoordinatorScope::default_backend")]
    pub scope: CoordinatorScope,
    pub reasoning: String,
}

impl CoordinatorScope {
    fn default_backend() -> Self { Self::Backend }
}
```

### 6. FSM — scope is observed but NOT acted on yet

Add a `tracing::info!` log line in the run loop when scope is
emitted, so during early W3-12h development we can verify
Coordinator is producing correct scope decisions for known goals:

```rust
tracing::info!(
    job_id = %job.id,
    route = ?decision.route,
    scope = ?decision.scope,
    "swarm: Coordinator decision recorded"
);
if matches!(decision.scope, CoordinatorScope::Frontend | CoordinatorScope::Fullstack) {
    tracing::warn!(
        job_id = %job.id,
        scope = ?decision.scope,
        "swarm: scope=frontend|fullstack detected; W3-12g still routes through backend chain — W3-12h activates scope-aware dispatch"
    );
}
```

The FSM's existing `BACKEND_BUILDER_ID` / `BACKEND_REVIEWER_ID`
constants are used unconditionally. No branching on scope yet.

### 7. Migration impact

**None.** `decision_json` from W3-12f is a JSON blob; the new
`scope` field lives inside the same JSON. No new column, no
new migration.

### 8. Tests

#### Profile-loader tests (mechanical)

- `bundled_six_profiles_present` → `bundled_eight_profiles_present` (rename + 2 new IDs in the assertion list).
- `commands/swarm.rs::profiles_list_returns_six_bundled` → `..._eight_bundled`.

#### Coordinator decision tests

- `parse_decision_with_scope_field` — `{"route":"execute_plan","scope":"frontend","reasoning":"..."}` parses with scope=Frontend.
- `parse_decision_missing_scope_defaults_to_backend` — `{"route":"execute_plan","reasoning":"..."}` (legacy shape) parses with scope=Backend.
- `coordinator_scope_serializes_snake_case` — Backend/Frontend/Fullstack → "backend"/"frontend"/"fullstack".
- `parse_decision_unknown_scope_rejected` — `"scope":"weird"` → AppError::SwarmInvoke.
- `coordinator_decision_round_trips_through_sqlite_with_scope` — write a Decision with scope=Frontend, reload via store, scope round-trips.

#### FSM tests

- `fsm_classify_emits_scope_in_decision` — mock Coordinator returns `{"route":"execute_plan","scope":"frontend",...}`; assert `stages[1].coordinator_decision.scope == Frontend`.
- `fsm_scope_frontend_logs_warning_but_uses_backend_chain` — mock with scope=Frontend; assert FSM still calls `backend-builder` + `backend-reviewer` (W3-12g contract: scope ignored). Captures the `tracing::warn!` via `tracing-subscriber::fmt::test::Captured` or similar — if too painful, just assert the run completes with the existing 6-stage flow (Scout + Classify + Plan + Build + Review + Test) and stages[3].specialist_id == "backend-builder".

#### Existing FSM regression

The 30+ mock-driven FSM tests from W3-12d/e/f need a small bulk update:
- `MockResponse` for `coordinator` profile must now emit a 3-field JSON: `{"route":"execute_plan","scope":"backend","reasoning":"mock"}`.
- The helper `execute_plan_decision_response()` produces this; all callers updated.
- Tests that hardcoded `"reviewer"` as a mock-id key must use `"backend-reviewer"`.

#### Persona compile-time test

- `bundled_eight_profiles_have_distinct_ids` — load all 8, assert no duplicate IDs.
- `bundled_eight_profiles_load_without_error` — implicit in `bundled_eight_profiles_present`, but a separate test asserting each parses cleanly individually helps surface frontmatter bugs.

Test count target: ≥325 (312 prior + ≥13 new). New ignored
integration tests: 0 (W3-12h adds the scope-driven smoke).

### 9. Bindings regen

`pnpm gen:bindings` adds:
- `CoordinatorScope` enum (`backend` | `frontend` | `fullstack`)
- `scope: CoordinatorScope` on `CoordinatorDecision`

The existing JS-side switch on `CoordinatorRoute` is unchanged
because route still has the same 2 variants.

`pnpm gen:bindings:check` exits 0 post-commit.

### 10. UI follow-up note

`SwarmJobDetail.tsx` from W3-14 + W3-14-followup renders
`coordinator_decision` data. Adding scope to the wire shape
means the existing UI may need a small follow-up to surface
scope as a pill or badge — but **out of scope for 12g**.
W3-12h's UI work is the natural place since scope only
matters once FSM acts on it.

## Out of scope

- ❌ FSM scope-aware dispatch (W3-12h: Backend → BB+BR;
  Frontend → FB+FR; Fullstack → BB+FB+BR+FR).
- ❌ Parallel Builder / Reviewer execution (W3-12h: tokio::join!
  for fan-out).
- ❌ Orchestrator profile / user-facing chat layer (W3-12i).
- ❌ Per-stage scope override ("force frontend reviewer for
  this run"). Post-W3.
- ❌ Migration of pre-12g `decision_json` rows. They deserialize
  with default scope=Backend; no rewrite needed.
- ❌ UI render of scope pill in SwarmJobDetail. W3-12h
  follow-up.
- ❌ Scope-aware Plan prompt template (Planner today produces
  one plan; W3-12h may split per-scope plans).
- ❌ Removing the old generic `reviewer` profile from any
  external configurations / docs the user might have. The
  workspace-override mechanism (W3-11 §2: `<app_data_dir>/agents/`)
  still works — a user with a custom `reviewer.md` keeps it
  (the bundled name no longer matches; the user's override
  takes priority for any ID they pick).

## Acceptance criteria

- [ ] `src-tauri/src/swarm/agents/{backend-reviewer,frontend-reviewer,frontend-builder}.md` exist, embedded via include_dir!.
- [ ] `src-tauri/src/swarm/agents/reviewer.md` REMOVED (renamed to backend-reviewer.md).
- [ ] `swarm:profiles_list` returns exactly 8 profiles, alphabetical: `backend-builder`, `backend-reviewer`, `coordinator`, `frontend-builder`, `frontend-reviewer`, `integration-tester`, `planner`, `scout`.
- [ ] `BACKEND_REVIEWER_ID` const replaces `REVIEWER_ID` in `swarm/coordinator/fsm.rs`. All call sites + tests updated.
- [ ] `CoordinatorScope` enum added; `CoordinatorDecision.scope` field present and `specta::Type`'d.
- [ ] `parse_decision` handles legacy shape (missing scope) with default=Backend.
- [ ] FSM logs `tracing::warn!` when scope=Frontend|Fullstack but continues with backend chain.
- [ ] Coordinator profile body updated with scope decision rules + few-shot examples covering scope variants.
- [ ] All Week-2 + Week-3-prior tests pass; target ≥325 passing.
- [ ] No new dep, no new migration, no `unsafe`, no `eprintln!`.
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check` exits 0 post-commit.
- [ ] No FSM behavior change (same Builder + Reviewer used regardless of scope) — verified by FSM regression tests.

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 1 pre-commit
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration regression (existing tests
# should pass unchanged because FSM behavior is unchanged):
cd src-tauri
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
cargo test --lib -- integration_research_only_real_claude --ignored --nocapture
```

## Notes / risks

- **Profile rename is a workspace-override-compatibility consideration.** A user who customized `reviewer.md` in their `<app_data_dir>/agents/` keeps that file; their custom version still loads under id `reviewer` if they kept the original `id:` field, but the FSM no longer references that id (it's now `backend-reviewer`). Their workspace override would need to be renamed or its `id:` field updated. Document in commit message; W3-12h or a doc commit can add a CHANGELOG-style entry.
- **`scope` field is on the wire from 12g but unused.** The FSM logs a warning when scope ≠ Backend; the warn is enough surface to verify Coordinator is producing correct scope classifications across the integration smoke before W3-12h ships the actual dispatch logic. This is "ship-the-data-first" pattern.
- **Cost note.** User directive 2026-05-06: cost not a concern. Coordinator profile getting bigger (more rules, more few-shot) → slightly higher token cost per Classify call (~$0.005 extra). Offset many times over by the quality gains of scope-aware reviews.
- **Bulk test fixture update.** ~30+ tests touch the mock-Coordinator response. Sub-agent must update the helper `execute_plan_decision_response()` AND any test that constructs a `MockResponse` for the Coordinator inline. Find-and-replace `"reviewer"` → `"backend-reviewer"` separately in mock-id keys.
- **Workspace-overridden profiles still take priority** — if a user drops `<app_data_dir>/agents/frontend-builder.md` with custom content, that overrides the bundled one (W3-11 §2 contract). No change to that mechanism in 12g.

## Sub-agent reminders

- Read this WP in full.
- Read `swarm/agents/{scout,planner,backend-builder,reviewer,integration-tester,coordinator}.md` for the persona patterns to mirror.
- Read `swarm/coordinator/{decision,fsm,job}.rs` for the existing decision/FSM shape.
- Read `app/src/lib/bindings.ts` for the existing wire shape so the regen diff is clean.
- DO NOT add a new dep.
- DO NOT add a new migration. The existing `decision_json` column carries the new `scope` field as a JSON sub-key.
- DO NOT change FSM dispatch logic. `BACKEND_REVIEWER_ID` always wins; scope is logged but ignored. W3-12h activates scope-aware dispatch.
- DO NOT remove or alter any existing FSM transition. The 6-stage chain is unchanged.
- DO NOT change `swarm:run_job` / `swarm:cancel_job` / `swarm:test_invoke` / `swarm:list_jobs` / `swarm:get_job` IPC signatures.
- DO NOT add `Orchestrator` profile or persona. That is W3-12i territory; mixing it into 12g would break the M-size target.
- Per `AGENTS.md`: one WP = one commit.

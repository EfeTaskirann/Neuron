---
id: WP-W3-12f
title: Coordinator FSM — single-shot Coordinator LLM brain (Option B routing)
owner: TBD
status: not-started
depends-on: [WP-W3-12d, WP-W3-12e]
acceptance-gate: "After SCOUT, FSM consults a 6th bundled profile (`coordinator.md`) for a single-shot routing decision: ResearchOnly (skip Plan/Build/Review/Test, return Scout's findings as the deliverable) or ExecutePlan (continue the 5-stage chain). Decision is parsed from JSON; persisted on the StageResult; surfaced to the UI via SwarmJobEvent. Hardcoded ExecutePlan fallback when parse fails."
---

## Goal

Add the Option B Coordinator LLM brain the architectural report
§5 prescribes: an on-demand, single-shot LLM call that decides
routing AT ONE specific decision point — research-only vs.
execute-plan — based on the goal + Scout's findings.

This WP intentionally ships exactly ONE Coordinator decision
(ResearchOnly vs ExecutePlan). Future Coordinator decisions
(skip Reviewer for trivial edits, retry strategy choice, etc.)
are W3-12g+.

## Why now

W3-12d/e shipped the quality gate + retry loop. They make the
swarm CORRECT. W3-12f makes it EFFICIENT for goals where the
full 5-stage chain is overkill:

- "Explain how the FSM transitions work" → Scout's findings are
  the answer. No Plan, no Build, no Review, no Test needed.
  Today: ~3-5 min + ~$0.10 wasted on empty Plan/Build outputs.
- "Add a doc comment to README" → Today runs full 5-stage chain
  with cargo build/check overhead. Future W3-12g could add
  "skip Test for doc-only edits"; this WP only handles the
  research-only short-circuit.

Estimated coverage of research-only goals: ~20-30% in typical
agent-team usage (asking-the-codebase questions). Saves ~70%
of those jobs' cost.

## Charter alignment

No tech-stack change. The Coordinator profile is just a 6th
`.md` profile in the existing bundled set. The FSM gains a new
state + transition.

## Scope

### 1. New bundled profile `coordinator.md`

`src-tauri/src/swarm/agents/coordinator.md`:

```yaml
---
id: coordinator
version: 1.0.0
role: Coordinator
description: Single-shot routing brain. Reads goal + Scout findings, emits a JSON CoordinatorDecision.
allowed_tools: ["Read", "Grep", "Glob"]
permission_mode: plan
max_turns: 4
---
```

Body — strict prompt engineering per architectural report §7.2:

- Persona: "You are the routing brain. You don't write code or
  produce content. You read the goal + Scout's findings and
  decide which downstream chain is appropriate."
- OUTPUT CONTRACT: emits ONLY a JSON object of shape
  `{ "route": "research_only" | "execute_plan", "reasoning": "..." }`.
- Decision rules (the persona's heuristic):
  - **research_only** if the goal is a question about the
    codebase that Scout's findings already answer ("explain X",
    "what does Y do", "list Z", "describe the architecture of
    A").
  - **execute_plan** if the goal asks for a code change
    ("add", "fix", "implement", "refactor", "update").
  - Default to **execute_plan** if uncertain — failing
    open is cheaper than misclassifying a build goal as
    research-only and silently doing nothing.
- Few-shot examples (3 each, approved/rejected style).
- Negative examples: no markdown fences, no preamble, no
  multi-paragraph reasoning before the JSON.
- "Sen Coordinator değil sen Specialist'sin" remind at end.

`max_turns: 4` because the decision should land in 1-2 turns
(read goal, maybe read 1-2 files for context, emit JSON).

### 2. New `swarm/coordinator/decision.rs` module

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorRoute {
    ResearchOnly,
    ExecutePlan,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct CoordinatorDecision {
    pub route: CoordinatorRoute,
    pub reasoning: String,
}

pub fn parse_decision(raw: &str) -> Result<CoordinatorDecision, AppError>;
```

`parse_decision` follows the same 4-step defense-in-depth as
`parse_verdict` (W3-12d):

1. Direct `serde_json::from_str` on trimmed input.
2. Strip markdown fence, retry.
3. First balanced `{...}` substring, retry.
4. Fail with `AppError::SwarmInvoke`.

OPTIONAL refactor: extract a generic `parse_robust_json<T>` in
`coordinator/json_helpers.rs` that both `parse_verdict` and
`parse_decision` use. Sub-agent's call — pick whichever is less
risky.

### 3. New `JobState::Classify` variant

Activate slot reserved for future routing in W3-12a's enum.
Update `next_state` and the FSM run loop. Migration impact:
add to `JobState::{as,from}_db_str`. No new SQL column.

### 4. FSM run loop addition

After SCOUT completes, run the Coordinator brain:

```rust
// SCOUT (existing)
let scout_text = ...;

// CLASSIFY (NEW — W3-12f)
let classify_prompt = render_classify_prompt(&goal, &scout_text);
let classify_outcome = self.run_stage_with_cancel(
    app, JobState::Classify, &coordinator, &classify_prompt, &job_id, &notify
).await;
let decision = match classify_outcome {
    StageOutcome::Ok(stage) => {
        let decision = parse_decision(&stage.assistant_text)
            .unwrap_or_else(|_| {
                tracing::warn!(
                    "Coordinator decision unparseable; defaulting to ExecutePlan"
                );
                CoordinatorDecision {
                    route: CoordinatorRoute::ExecutePlan,
                    reasoning: "fallback: brain output unparseable".into(),
                }
            });
        // Record the parsed decision on the StageResult for
        // visibility. New optional field on StageResult.
        let stage_with_decision = StageResult {
            coordinator_decision: Some(decision.clone()),
            ..stage
        };
        self.registry.update(&job_id, |j| j.stages.push(stage_with_decision)).await?;
        decision
    }
    // Stage error or cancel → existing finalize paths.
    ...
};

// Branch on the route.
match decision.route {
    CoordinatorRoute::ResearchOnly => {
        // Skip Plan/Build/Review/Test entirely. Scout's findings
        // ARE the deliverable. Finalize as Done.
        self.finalize_done(...).await?;
        return self.build_outcome(&job_id);
    }
    CoordinatorRoute::ExecutePlan => {
        // Fall through to the existing Plan/Build/Review/Test
        // chain (unchanged).
    }
}

// PLAN (existing) ... continues
```

### 5. New prompt template `CLASSIFY_PROMPT_TEMPLATE`

```rust
const CLASSIFY_PROMPT_TEMPLATE: &str = "Hedef:\n\
\n\
{goal}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
Bu hedef için uygun rotayı seç. Çıktı sadece bir JSON objesi olmalı:\n\
{{ \"route\": \"research_only\" | \"execute_plan\", \"reasoning\": \"...\" }}\n";
```

`render_classify_prompt(goal, scout_output)` is a free fn next
to the existing render helpers.

### 6. New wire field `StageResult.coordinator_decision`

```rust
pub struct StageResult {
    // existing fields ...
    pub verdict: Option<Verdict>,            // populated for Review/Test
    pub coordinator_decision: Option<CoordinatorDecision>,  // populated for Classify
}
```

`Job` gains nothing new (the brain decision is on the Classify
StageResult, not the Job-level summary).

### 7. New `SwarmJobEvent::DecisionMade` variant

Optional but valuable for UI. Fires after Classify so the UI
can render "research-only" or "executing plan" pill before
the next stage starts.

```rust
pub enum SwarmJobEvent {
    // existing 6 kinds ...
    DecisionMade {
        job_id: String,
        decision: CoordinatorDecision,
    },
}
```

UI surfacing is W3-14 follow-up; this WP just fires the event.

### 8. Persistence

`StageResult.coordinator_decision` follows the same JSON-column
pattern as `verdict_json`. New migration `0008_swarm_decision.sql`:

```sql
ALTER TABLE swarm_stages ADD COLUMN decision_json TEXT;
```

Migration count 7 → 8. Update `db.rs` count tests.

### 9. 6-profile bundle is the new contract

`swarm:profiles_list` returns 6 profiles after this WP:
`backend-builder`, `coordinator`, `integration-tester`,
`planner`, `reviewer`, `scout`.

Update profile-loader test from `bundled_five_profiles_present`
→ `bundled_six_profiles_present` (or add a new sibling test
keeping the 5-name one for incremental docs).

### 10. Tests

#### Unit (mock-driven)

- `parse_decision_direct_object`
- `parse_decision_with_json_fence`
- `parse_decision_unparseable_returns_error`
- `coordinator_route_serializes_snake_case` — round-trip both variants.
- `fsm_classify_research_only_skips_to_done` — mock
  Coordinator returns `route: research_only`; FSM finalizes Done
  after Classify; stages.len() == 2 (Scout + Classify). No
  Plan/Build/Review/Test stages.
- `fsm_classify_execute_plan_continues_full_chain` — mock
  Coordinator returns `route: execute_plan`; full 5-stage
  chain runs; stages.len() == 6 (Scout + Classify + Plan +
  Build + Review + Test).
- `fsm_classify_unparseable_falls_back_to_execute_plan` —
  mock Coordinator returns garbage; FSM falls through to
  Plan/Build/Review/Test with a warning logged.
- `decision_made_event_fires_after_classify` — assert
  `DecisionMade` event arrives between
  `StageCompleted(Classify)` and the next `StageStarted`.
- `coordinator_decision_persists_on_stage_result` — drive
  through, reload via `store::get_job_detail`, assert
  `stages[1].coordinator_decision` is Some with the right
  variant.
- `classify_prompt_includes_goal_and_scout_findings` —
  verifying the template substitution.

#### Integration test (`#[ignore]`)

`integration_research_only_real_claude` — real-claude run
with a goal like `"Explain how the FSM transitions work in
src/swarm/coordinator/fsm.rs"`. Expect:
- Final state Done
- stages.len() == 2 (Scout + Classify only)
- coordinator_decision.route == ResearchOnly
- Wall-clock < 60s (vs. 200s+ for full chain)

This is the ROI demo: the same goal would have run all 5
stages without W3-12f.

#### Existing FSM regression

All W3-12d/e tests currently mock 5 stages. Update them to
mock 6 stages (insert Classify after Scout, mock returns
`execute_plan` so the existing flow is preserved). Most tests
need 1-2 lines added (mock Classify response).

### 11. Bindings regen

`pnpm gen:bindings` adds:
- `CoordinatorRoute` enum (research_only | execute_plan)
- `CoordinatorDecision` struct
- `coordinator_decision?: CoordinatorDecision` field on
  `StageResult`
- `DecisionMade` variant on `SwarmJobEvent`

`pnpm gen:bindings:check` exits 0 post-commit.

## Out of scope

- ❌ Multiple Coordinator decisions in a single job. One
  decision (Classify) at the start; future decisions are
  W3-12g+.
- ❌ Coordinator deciding Reviewer-skip, Tester-skip, retry
  strategy, profile-set narrowing. All future W3-12g+.
- ❌ Per-decision policy tuning (e.g. "always run Reviewer
  even on research-only"). Out of scope; current Charter
  trusts the Coordinator's judgment.
- ❌ User-overridable routing ("force execute even if
  classified research-only"). Post-W3.
- ❌ Coordinator running BEFORE Scout (no scout output). The
  FSM always runs Scout first; Coordinator decides whether to
  continue past Scout.
- ❌ UI surfacing of `DecisionMade` event / route pill. Event
  fires; rendering is W3-14 follow-up.

## Acceptance criteria

- [ ] `swarm/agents/coordinator.md` exists, 6th bundled profile.
- [ ] `swarm/coordinator/decision.rs` exists with the 2 types
      + `parse_decision`.
- [ ] `JobState::Classify` reachable in FSM; `next_state`
      handles it.
- [ ] FSM Scout → Classify → (ResearchOnly→Done | ExecutePlan→
      Plan→Build→Review→Test→Done).
- [ ] `StageResult.coordinator_decision: Option<CoordinatorDecision>`
      populated for Classify stages.
- [ ] `SwarmJobEvent::DecisionMade` fires post-Classify with
      the parsed decision.
- [ ] Migration `0008_swarm_decision.sql` adds `decision_json`
      column to `swarm_stages`.
- [ ] Migration count test bumps 7 → 8.
- [ ] `swarm:profiles_list` returns exactly 6 profiles.
- [ ] Decision round-trips through SQLite.
- [ ] Unparseable Coordinator output → log warn + fall back to
      ExecutePlan (do NOT fail the job).
- [ ] All Week-2 + Week-3-prior tests still pass; target ≥305
      passing (293 prior + ≥12 new).
- [ ] No new dep, no `unsafe`, no `eprintln!`.
- [ ] Integration test (`#[ignore]`d) compiles.
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check`
      exits 0 post-commit.

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 1 pre-commit
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration smokes (post-commit):
cd src-tauri
cargo test --lib -- integration_research_only_real_claude --ignored --nocapture
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
```

## Notes / risks

- **Default-fail-open on parse error.** Unparseable Coordinator
  output → ExecutePlan (full chain). The cost of a misclassified
  research goal is "spent ~$0.10 on wasted Plan/Build/Review/Test"
  — annoying but recoverable. The cost of a misclassified
  execute goal as research-only is "the user thinks the job
  succeeded but no code was written" — much worse. Default to
  ExecutePlan.
- **Coordinator profile prompt is critical.** Borderline goals
  ("show me a simpler way to do X" — research or execute?)
  will hinge on the persona's heuristics. Document the rules
  clearly in the body. Future tuning is a profile-edit, not
  a code change.
- **Classify cost.** ~$0.01-0.03 per job (one extra LLM call).
  ROI positive iff >5% of jobs are classified research-only
  (saving ~$0.10 each).
- **`unwrap_or_else` for parse fallback.** This WP is the only
  place we accept malformed JSON without erroring. Documented
  inline; tests assert the fallback fires.
- **`Job.stages` length implications.** Today 5 (Scout/Plan/
  Build/Review/Test); after this WP the floor is 2 (research)
  or 6 (execute, with Classify between Scout and Plan). UI
  consumers must not assume a fixed length.
- **Cancel during Classify.** Same select-against-notify
  pattern as other stages. No new logic.

## Sub-agent reminders

- Read this WP in full.
- Read `swarm/coordinator/{fsm,job,verdict}.rs` (W3-12a/b/c/d/e)
  for the FSM/persistence patterns.
- Read `swarm/agents/reviewer.md` and `integration-tester.md`
  for the persona format.
- Read the architectural report §11.4 (Single Claude Instance
  vs Multiple Processes) for the Option B rationale.
- DO NOT add a new dep.
- DO NOT introduce additional Coordinator decisions beyond
  Classify (research_only vs execute_plan).
- DO NOT increase the number of Coordinator calls per job —
  exactly one Classify call per job.
- DO NOT change the FSM's transition table beyond the new
  Scout → Classify edge and the Classify → (Done | Plan)
  branch.
- Per AGENTS.md: one WP = one commit.

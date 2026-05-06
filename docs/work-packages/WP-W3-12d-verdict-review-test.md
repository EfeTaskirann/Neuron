---
id: WP-W3-12d
title: Coordinator FSM — REVIEW + TEST states + Verdict schema + robust JSON parser
owner: TBD
status: not-started
depends-on: [WP-W3-12a, WP-W3-12b, WP-W3-12c]
acceptance-gate: "FSM walks SCOUT → PLAN → BUILD → REVIEW → TEST → DONE on the happy path. Reviewer + IntegrationTester emit `Verdict { approved, issues, summary }` JSON; robust parser tolerates markdown fences. `Verdict.approved=false` at either gate finalizes the job as Failed with `last_verdict` populated. NO retry loop in this WP (Failed-on-reject is the W3-12d contract; retry is W3-12e)."
---

## Goal

Add the quality gate the architectural report §5.3 calls out:
two LLM-driven judges (REVIEW for code review, TEST for
integration testing) sit between BUILD and DONE. Each emits a
JSON Verdict; the FSM gates on `approved`.

This WP intentionally **does NOT** add the retry feedback loop
or the Coordinator LLM brain. Those land separately:

- **W3-12d (this WP)**: REVIEW + TEST + Verdict + parser. On
  reject → Failed. User can click Rerun in the W3-14 UI for a
  manual retry.
- **W3-12e (future)**: automatic retry loop with feedback piped
  back to Planner, capped at `MAX_RETRIES=2`.
- **W3-12f (future)**: Coordinator LLM brain (Option B from the
  W3-12a §"Architectural rationale") — on-demand single-shot
  routing decisions.

Splitting keeps W3-12d at M-size; the retry loop is itself an
M-sized concern (state machine semantics + feedback-prompt
template + UI surfacing of retry attempts).

## Why now

The W3-14 UI shows green-pill "Done" for any Builder output
that didn't crash, regardless of correctness. That's the
demo-stopper: a swarm that "succeeds" but produces broken code
is worse than no swarm at all. Reviewer + IntegrationTester are
the gate that separates "ran without errors" from "passes
quality bar."

## Scope

### 1. Two new bundled profiles

#### `src-tauri/src/swarm/agents/reviewer.md`

```yaml
---
id: reviewer
version: 1.0.0
role: Reviewer
description: Read-only code reviewer. Emits a JSON Verdict over the Builder's output.
allowed_tools: ["Read", "Grep", "Glob"]
permission_mode: plan
max_turns: 8
---
# Reviewer

(persona body — short imperative; tells reviewer to read what Builder wrote, evaluate for correctness/quality/style, and emit ONLY the JSON Verdict in this exact shape:)

```json
{
  "approved": true|false,
  "issues": [
    { "severity": "high|med|low", "file": "path", "line": 42, "msg": "..." }
  ],
  "summary": "one-paragraph overall assessment"
}
```

Reviewer reads Builder's changes (Read), greps for related callers if needed, and decides:
- approved=true if the change is correct, idiomatic, doesn't break existing tests
- approved=false if there are high-severity issues (broken logic, security, or style violations the codebase explicitly forbids per CLAUDE.md / Charter)

CRITICAL: reviewer ALWAYS emits valid JSON. No markdown fences. No conversational preamble. The first character of the response is `{`.

#### `src-tauri/src/swarm/agents/integration-tester.md`

```yaml
---
id: integration-tester
version: 1.0.0
role: IntegrationTester
description: Runs project tests/builds and emits a JSON Verdict on the result.
allowed_tools: ["Read", "Bash(cargo *)", "Bash(pnpm *)", "Bash(npm test *)", "Bash(pytest *)"]
permission_mode: acceptEdits
max_turns: 12
---
# IntegrationTester

(persona — runs the appropriate test suite for the project (cargo test / pnpm test / pytest), captures pass/fail, emits a Verdict.)

approved=true iff all tests in the relevant suite pass.
approved=false iff any failure, with the failing-test names in `issues`.

Same JSON-only output discipline as reviewer.

### 2. `Verdict` schema

`src-tauri/src/swarm/coordinator/verdict.rs` (new module):

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum VerdictSeverity { High, Med, Low }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct VerdictIssue {
    pub severity: VerdictSeverity,
    pub file: Option<String>,
    pub line: Option<u32>,
    #[serde(rename = "msg")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct Verdict {
    pub approved: bool,
    pub issues: Vec<VerdictIssue>,
    pub summary: String,
}

impl Verdict {
    pub fn rejected(&self) -> bool { !self.approved }
}
```

### 3. Robust JSON parser (`verdict::parse`)

Per architectural report §7.1 — defense in depth against markdown
fence wrapping:

```rust
pub fn parse_verdict(raw: &str) -> Result<Verdict, AppError> {
    // 1. Try direct parse.
    if let Ok(v) = serde_json::from_str::<Verdict>(raw.trim()) {
        return Ok(v);
    }
    // 2. Strip ```json ... ``` or ``` ... ``` fences.
    if let Some(stripped) = strip_markdown_fence(raw) {
        if let Ok(v) = serde_json::from_str::<Verdict>(stripped) {
            return Ok(v);
        }
    }
    // 3. Find first balanced `{...}` substring.
    if let Some(sub) = first_balanced_json_object(raw) {
        if let Ok(v) = serde_json::from_str::<Verdict>(sub) {
            return Ok(v);
        }
    }
    // 4. Fail.
    Err(AppError::SwarmInvoke(format!(
        "could not parse Verdict from assistant text: {}",
        truncate_chars(raw, 400)
    )))
}
```

`strip_markdown_fence` and `first_balanced_json_object` are
helpers in the same module, well-unit-tested. Balanced parser
counts `{`/`}` accounting for strings (single + double-quoted)
and escape characters.

### 4. JobState activation

`JobState::Review` and `JobState::Test` lose their
`debug_assert!(false)` guards in `next_state`. Updated
transition table:

```
INIT          → SCOUT  (always)
SCOUT(ok)     → PLAN
SCOUT(err)    → FAILED
PLAN(ok)      → BUILD
PLAN(err)     → FAILED
BUILD(ok)     → REVIEW
BUILD(err)    → FAILED
REVIEW(ok+approved)   → TEST
REVIEW(ok+rejected)   → FAILED
REVIEW(err)           → FAILED
TEST(ok+approved)     → DONE
TEST(ok+rejected)     → FAILED
TEST(err)             → FAILED
DONE / FAILED → terminal
```

The `(ok+approved)` / `(ok+rejected)` distinction is new — the
FSM unwraps the Verdict and branches on `approved`.

### 5. FSM additions

`run_job` gains two more stage runs after BUILD:

```rust
// REVIEW stage
let review_prompt = render_review_prompt(&goal, &plan_text, &build_text);
let review_outcome = self
    .run_stage_with_cancel(app, JobState::Review, &reviewer, &review_prompt, &job_id, &notify)
    .await;
let review_text = match review_outcome {
    StageOutcome::Ok(stage) => { /* push, parse Verdict, branch */ }
    ...
};
let review_verdict = parse_verdict(&review_text)?;
if review_verdict.rejected() {
    self.finalize_failed_with_verdict(&job_id, &workspace_id, review_verdict.clone())?;
    emit StageCompleted, then Finished
    return outcome;
}
emit StageCompleted with the verdict in StageResult.verdict (new field) ...

// TEST stage — same shape
let test_prompt = render_test_prompt(&goal, &build_text);
... etc
```

`run_stage_with_cancel` is the existing W3-12c helper renamed
slightly (currently inline in `run_stage`; this WP just keeps
the existing pattern with verdict handling at the call site).

### 6. New fields on `StageResult` and `Job`

`StageResult` gains:
```rust
pub verdict: Option<Verdict>,  // populated for Review and Test stages; None for others
```

`Job` and `JobOutcome` gain:
```rust
pub last_verdict: Option<Verdict>,  // populated when terminated by a verdict reject
```

Wire-shape changes ripple through `JobDetail`, `JobSummary`
(unchanged — summary is slim), and `bindings.ts`. Frontend
W3-14 doesn't render the verdict yet but the data is on the
wire for a future polish WP.

### 7. Stage prompt templates

`render_review_prompt(goal, plan_text, build_text)`:

```
Görev: {goal}

Plan:
{plan}

Builder'ın çıktısı:
{build}

Bu kodu ve değişiklikleri review et. Verdict'i tam olarak
şu JSON şemasında ver, başka hiçbir şey yazma:

{ "approved": <bool>, "issues": [...], "summary": "..." }
```

`render_test_prompt(goal, build_text)`:

```
Görev: {goal}

Builder'ın çıktısı:
{build}

İlgili test suite'ini çalıştır (cargo test / pnpm test).
Verdict'i şu JSON şemasında ver:

{ "approved": <bool>, "issues": [...], "summary": "..." }
```

Both follow the architectural report §7.2 strict-prompt-engineering
discipline: explicit OUTPUT CONTRACT, no fence allowed, JSON-only.

### 8. Persistence

`swarm_stages.assistant_text` already stores the raw text. Add
a new column for the parsed verdict:

```sql
-- migration 0007_swarm_verdict.sql
ALTER TABLE swarm_stages ADD COLUMN verdict_json TEXT;  -- nullable; only set for Review/Test stages
ALTER TABLE swarm_jobs ADD COLUMN last_verdict_json TEXT;  -- nullable; only set for verdict-rejected jobs
```

Migration count goes 6 → 7. Update the `db.rs` count tests.

`store::insert_stage` now serializes the optional Verdict to
JSON. `store::update_job` adds the `last_verdict` field.
`store::list_jobs` and `get_job_detail` deserialize.

### 9. Tests

#### Verdict parser tests (no claude needed)

- `parse_verdict_direct_object` — `{"approved":true,"issues":[],"summary":"OK"}` parses.
- `parse_verdict_with_json_fence` — ```json ... ``` wrapping parses.
- `parse_verdict_with_unlabeled_fence` — ``` ... ``` wrapping parses.
- `parse_verdict_with_preamble_and_json` — "Here's my verdict: { ... }" parses (balanced-substring path).
- `parse_verdict_rejected_with_issues` — verdict with non-empty issues array parses.
- `parse_verdict_severity_variants` — high / med / low all round-trip.
- `parse_verdict_invalid_returns_error` — garbage text → `AppError::SwarmInvoke`.
- `parse_verdict_balanced_braces_with_strings` — `{"summary":"a } b"}` parses (string-aware brace counting).
- `parse_verdict_unicode_safe` — Turkish / emoji in summary survives parser.

#### FSM tests with mock Verdicts

- `fsm_walks_five_stages_on_approved_path` — mock returns OK assistant_text, plus mock `parse_verdict` always returns `approved=true`. Final state: Done. stages.len() == 5.
- `fsm_review_rejection_finalizes_failed_with_verdict` — mock reviewer returns `approved=false`; final_state == Failed; outcome.last_verdict.is_some(); outcome.last_verdict.approved == false.
- `fsm_test_rejection_finalizes_failed_with_verdict` — same shape, but at the Test gate.
- `fsm_review_unparseable_finalizes_failed` — mock reviewer returns "lol idk"; outcome.last_error mentions "could not parse Verdict".
- `fsm_review_skipped_when_build_fails` — mock builder errors; final_state == Failed; review/test stages not in stages.
- `verdict_persists_across_app_restart` — write a Failed job with last_verdict via the registry; reload via store::get_job_detail; verdict round-trips.

#### Integration test (`#[ignore]`)

- `integration_full_chain_real_claude_with_verdict` — real-claude end-to-end: scout → planner → backend-builder → reviewer → integration-tester. Goal is the canonical `profile_count` add. Expect Done (Reviewer should approve a one-line method add, IntegrationTester runs `cargo check` or `cargo test` — depends on persona body crafting). Time budget 5 × 180s = 900s worst-case; typical 2-4 min. `#[ignore]`.

#### Existing FSM regression tests

All W3-12a/b/c FSM tests must still pass — the 3-state happy
path tests need updating because the FSM now does 5 stages.
Either extend the mocks to cover all 5, or keep the existing
tests as "3-stage flow with reviewer/tester mocked to auto-
approve and emit empty Verdict". Pick whichever is less churn.

Target test delta: ≥18 unit + 1 ignored integration. New
baseline ≥272.

### 10. Bindings regen

`pnpm gen:bindings` adds:
- `Verdict`, `VerdictIssue`, `VerdictSeverity` types
- `last_verdict?: Verdict` field on `JobDetail`, `JobOutcome`
- `verdict?: Verdict` field on `StageResult`

`pnpm gen:bindings:check` exits 0 post-commit.

### 11. UI follow-up note

W3-14 doesn't render the verdict. A small future WP (or
follow-up commit) can add a "Verdict" subsection to
`SwarmJobDetail.tsx` for jobs with `last_verdict !== null`.
Out of scope for 12d — this WP ships the data; UI ships next.

## Out of scope

- ❌ **Retry feedback loop.** On Verdict reject → Failed in 12d.
  Automatic retry with `MAX_RETRIES=2` is W3-12e.
- ❌ **Coordinator LLM brain (Option B).** Routing decisions
  remain hardcoded in 12d's transition table. Brain is W3-12f.
- ❌ Per-Verdict-issue surfacing in W3-14 UI. Data ships; UI is
  a follow-up.
- ❌ Streaming a Verdict mid-stage (i.e. partial JSON deltas).
  Verdict arrives in the final `result` event.
- ❌ Verdict-driven cancel (e.g. high-severity issue triggers
  early termination). FSM only branches on `approved`.
- ❌ User-overridable Verdict ("approve anyway" UX). Post-W3.

## Acceptance criteria

- [ ] `src-tauri/src/swarm/agents/{reviewer,integration-tester}.md` exist + embedded via include_dir!
- [ ] `swarm/coordinator/verdict.rs` exists with the 3 types + `parse_verdict` + helpers
- [ ] `JobState::{Review,Test}` reachable via FSM; `next_state` debug_assert removed for these
- [ ] `StageResult.verdict: Option<Verdict>` populated for Review/Test stages
- [ ] `Job.last_verdict` and `JobOutcome.last_verdict` populated when terminated by a Verdict reject
- [ ] Migration `0007_swarm_verdict.sql` adds two ALTER TABLE columns
- [ ] Migration count test bumps from 6 → 7
- [ ] Verdict round-trips through SQLite (insert/select)
- [ ] All Week-2 + Week-3-prior tests still pass; target ≥272 passing
- [ ] No new dep, no `unsafe`, no `eprintln!`
- [ ] Integration test (`#[ignore]`d) compiles
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check` exits 0 post-commit
- [ ] `swarm:profiles_list` returns exactly 5 profiles on a fresh install (3 prior + reviewer + integration-tester)

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 1 pre-commit
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration smokes:
cd src-tauri
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
cargo test --lib -- integration_persistence_survives_real_claude_chain --ignored --nocapture
cargo test --lib -- integration_fsm_drives_real_claude_chain --ignored --nocapture
cargo test --lib -- integration_cancel_during_real_claude_chain --ignored --nocapture
```

## Notes / risks

- **Reviewer prompt discipline is critical.** Strict OUTPUT
  CONTRACT in the persona body + few-shot example + negative
  examples (no fence, no preamble). The robust parser is the
  fallback, but the prompt should produce direct-parseable
  output 95%+ of the time.
- **IntegrationTester runs `cargo test` or `pnpm test`.** This
  takes 30-90s on the canonical goal. Builder profile already
  has Bash(cargo*) / Bash(pnpm*). Tester reuses the same
  whitelist plus `Bash(npm test *)` and `Bash(pytest *)` for
  Python projects.
- **Migration ordering.** This is `0007_swarm_verdict.sql`,
  after `0006_swarm_jobs.sql` from W3-12b. Do NOT reorder.
  ALTER TABLE on SQLite is restricted; ADD COLUMN with a
  nullable default is the only safe op here.
- **JobOutcome.last_verdict deserves a typed field**, not
  shoved into `last_error`. Test asserts `last_error == None`
  on Verdict-rejection (the verdict IS the structured error).
- **Verdict.approved=false at REVIEW skips TEST entirely.**
  No point running tests on rejected code; saves a 30-90s
  Tester invocation.
- **Test budget.** Running cargo test in IntegrationTester from
  inside cargo test (the integration smoke) is fine on
  Windows/macOS; the inner cargo respects `CARGO_TARGET_DIR`
  if set. The test sets a 180s/stage budget, 5 stages = 900s
  worst-case but typical 3-5 min.

## Sub-agent reminders

- Read this WP in full.
- Read `src-tauri/src/swarm/coordinator/{fsm,job}.rs` (W3-12a/b/c)
  for the existing FSM + persistence shape.
- Read `src-tauri/src/swarm/agents/scout.md` and `planner.md` and
  `backend-builder.md` for the persona format. The two new
  profiles follow the same frontmatter contract.
- Read the architectural report `report/...` §5.3 (Gate Logic),
  §7.1 (Robust JSON Extraction), §7.2 (Strict Prompt
  Engineering) before crafting the reviewer / tester personas
  and the parser.
- DO NOT add a retry loop in this WP. Failed-on-reject is the
  contract.
- DO NOT add a Coordinator LLM brain in this WP.
- DO NOT change W3-12c's event surface beyond adding new
  Stage{Started,Completed} for Review/Test (which fall out of
  the existing run_stage helper naturally).
- DO NOT add new deps. `serde_json::Value` is already in tree
  for the parser fallback path if needed.
- Per AGENTS.md: one WP = one commit.

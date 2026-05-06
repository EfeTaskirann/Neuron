---
id: WP-W3-12e
title: Coordinator FSM — retry feedback loop (Verdict.rejected → Planner with feedback, MAX_RETRIES=2)
owner: TBD
status: not-started
depends-on: [WP-W3-12d]
acceptance-gate: "When Reviewer or IntegrationTester emits Verdict.rejected, FSM increments retry_count and transitions back to PLAN with the issues + previous plan piped into the planner prompt. Builder retries up to MAX_RETRIES=2 times. If the final attempt is still rejected, Job finalizes Failed with last_verdict + retry_count==2. New SwarmJobEvent::RetryStarted fires per retry so future UI can render attempt counters."
---

## Goal

Close the loop W3-12d opened. Today a Reviewer-rejected job
goes straight to Failed and the user manually clicks Rerun.
This WP makes the FSM auto-retry within a `MAX_RETRIES=2`
budget, with the Verdict's issues fed back to the Planner so
each iteration is informed by the previous failure.

This makes the swarm runtime actually self-correcting for
small mistakes. Real-world Builder output often has one or two
tiny issues (missing import, off-by-one, wrong feature flag);
a single retry round catches them.

## Why now

W3-12d ships the gate; W3-12e ships the loop that makes the
gate productive. Without retry, every Reviewer rejection is a
context-switch back to the human. With MAX_RETRIES=2, ~80% of
rejections (per the architectural report §5.3 estimate) self-
heal in one extra round.

`retry_count: u32` is already a field on `Job` (W3-12a),
already persisted (W3-12b). `MAX_RETRIES = 2` is already a
const (W3-12a). This WP adds the *consumer*: the FSM branch
that actually uses them.

## Scope

### 1. New FSM transition: REVIEW/TEST(rejected) → PLAN(retry)

In `swarm/coordinator/fsm.rs`, the run loop's REVIEW and TEST
verdict-handling branches change:

```rust
// REVIEW gate (existing):
if review_verdict.rejected() {
    self.finalize_failed_with_verdict(&job_id, &workspace_id, review_verdict)?;
    return self.build_outcome(&job_id);
}

// REVIEW gate (W3-12e):
if review_verdict.rejected() {
    let job = self.registry.get(&job_id).expect("job exists");
    if job.retry_count < MAX_RETRIES {
        // Fire RetryStarted event, increment counter, jump back to PLAN
        // with the verdict issues + previous plan piped into the prompt.
        self.registry.update(&job_id, |j| {
            j.retry_count += 1;
            j.state = JobState::Plan;
            j.last_verdict = Some(review_verdict.clone());  // last verdict
                                                            // ALWAYS reflects the most
                                                            // recent gate decision
        }).await?;
        emit_swarm_event(app, &job_id, &SwarmJobEvent::RetryStarted {
            job_id: job_id.clone(),
            attempt: job.retry_count + 1,  // 1-indexed for UI ("attempt 2 of 3")
            max_retries: MAX_RETRIES,
            triggered_by: JobState::Review,
            verdict: review_verdict.clone(),
        });
        // Loop back to the PLAN stage.
        continue 'fsm_loop;  // see §3 for the loop restructure
    } else {
        self.finalize_failed_with_verdict(&job_id, &workspace_id, review_verdict)?;
        return self.build_outcome(&job_id);
    }
}
```

Symmetric for TEST.

### 2. New event: `SwarmJobEvent::RetryStarted`

Add to `swarm/coordinator/job.rs`:

```rust
#[derive(...)]
pub enum SwarmJobEvent {
    // ... existing 5 kinds ...
    RetryStarted {
        job_id: String,
        attempt: u32,            // 1-indexed (first retry is "attempt 2")
        max_retries: u32,        // typically 2
        triggered_by: JobState,  // Review or Test
        verdict: Verdict,        // the rejection reasoning
    },
}
```

UI (W3-14 follow-up) can render "🔄 Attempt 2 of 3" pills and
expose the previous verdict's issues. This WP only fires the
event; rendering is post-W3.

### 3. FSM run loop restructure

The existing `run_job_inner` is a flat sequence of stages.
Retry needs a loop. Refactor to:

```rust
async fn run_job_inner(...) -> Result<JobOutcome, AppError> {
    // ... pre-stage setup (workspace lock, cancel notify, Started event) ...

    'retry_loop: loop {
        // SCOUT runs ONCE — only on the first attempt.
        let scout_text = if !scout_completed {
            self.run_scout_stage(...).await?
        } else {
            self.cached_scout_text.clone()
        };

        // PLAN runs every retry; prompt varies based on retry_count.
        let plan_prompt = if retry_count == 0 {
            render_plan_prompt(&goal, &scout_text)
        } else {
            render_retry_plan_prompt(&goal, &scout_text, &previous_plan, &previous_verdict)
        };
        let plan_text = self.run_plan_stage(plan_prompt, ...).await?;

        // BUILD runs every retry.
        let build_text = self.run_build_stage(plan_text, ...).await?;

        // REVIEW runs every retry.
        let review_verdict = self.run_review_stage(...).await?;
        if review_verdict.rejected() && retry_count < MAX_RETRIES {
            self.fire_retry_event(JobState::Review, review_verdict);
            continue 'retry_loop;
        } else if review_verdict.rejected() {
            return self.finalize_failed_with_verdict(...);
        }

        // TEST runs every retry.
        let test_verdict = self.run_test_stage(...).await?;
        if test_verdict.rejected() && retry_count < MAX_RETRIES {
            self.fire_retry_event(JobState::Test, test_verdict);
            continue 'retry_loop;
        } else if test_verdict.rejected() {
            return self.finalize_failed_with_verdict(...);
        }

        // All gates passed.
        return self.finalize_done(...);
    }
}
```

`scout_completed` short-circuits Scout on retry — its findings
don't change between attempts, and re-running it costs ~10s
of redundant LLM time. Plan/Build/Review/Test all re-run with
fresh prompts and fresh subprocesses (per-invoke discipline
preserved).

### 4. New prompt template `RETRY_PLAN_PROMPT_TEMPLATE`

```rust
const RETRY_PLAN_PROMPT_TEMPLATE: &str = "Hedef: {goal}\n\
\n\
Scout bulguları:\n\
\n\
{scout_output}\n\
\n\
ÖNCEKİ DENEMENİZ {gate} aşamasında REDDEDİLDİ. Reviewer/Tester'ın bulduğu sorunlar:\n\
\n\
{verdict_issues}\n\
\n\
Önceki plan (reddedildi):\n\
\n\
{previous_plan}\n\
\n\
Bu sorunları çözecek yeni bir plan üret. Yalnızca reddedilen\n\
sorunlara odaklan; mevcut başarılı kısımları yeniden tasarlama.\n";
```

`{verdict_issues}` is rendered as a human-readable bullet list
of `{severity}: {file}:{line} — {message}` from the rejected
Verdict. NOT raw JSON — Planner reads prose better than JSON
in its input.

`render_retry_plan_prompt(goal, scout, prev_plan, verdict, gate)`
is a new helper alongside `render_plan_prompt`. The `gate`
param is the `JobState` (Review or Test) that triggered the
retry, formatted as "Reviewer" or "IntegrationTester" in the
prompt.

### 5. Persistence

`retry_count` is already in `swarm_jobs.retry_count`. The
`update_job` SQL helper from W3-12b already serializes it.
Verify by adding a unit test that asserts `retry_count==1`
after a single retry round.

`Job.last_verdict` is set on every Verdict-rejection (whether
mid-retry or terminal-fail). Documented behavior: it always
reflects the *most recent* gate's verdict, not just the
final-fail one.

### 6. Cancel during retry

If `swarm:cancel_job` fires while the FSM is in mid-retry, the
existing W3-12c cancel logic catches it (the `tokio::select!`
in each stage is preserved). The `Cancelled` event's
`cancelled_during` field reports whichever stage of whichever
attempt was running. No new logic needed.

### 7. Tests

#### Unit tests (mock-driven)

- `fsm_review_reject_retries_within_budget` — Mock first
  Reviewer call returns rejected, second returns approved. Mock
  Tester returns approved. Final state Done, retry_count=1,
  stages.len() = 5 + 4 (re-runs of Plan/Build/Review/Test, NOT
  Scout). RetryStarted event fires once.
- `fsm_review_reject_exhausts_retries` — Mock Reviewer returns
  rejected on all 3 attempts. Final state Failed, retry_count=2,
  last_verdict.is_some(), stages.len() = 5 + 4 + 4. RetryStarted
  fires twice.
- `fsm_test_reject_retries_then_passes` — Mock Reviewer always
  approves; mock Tester rejects first then approves. Final state
  Done, retry_count=1.
- `fsm_test_reject_exhausts_retries` — Mock Tester rejects all 3.
- `fsm_scout_runs_once_across_retries` — count Scout invocations
  in the mock; exactly 1 even with retries.
- `retry_plan_prompt_includes_verdict_issues` — assert the
  Planner's prompt on attempt 2 contains the issues bullet list
  AND the previous-plan text.
- `retry_plan_prompt_omitted_on_first_attempt` — first-attempt
  Planner prompt is the original `render_plan_prompt`, not the
  retry variant.
- `retry_started_event_fires_with_correct_attempt_number` —
  attempt=2 on first retry, attempt=3 on second.
- `retry_count_persists_across_app_restart` — write a
  retry_count=1 job via the registry, reload, assert it
  round-trips.
- `cancel_during_retry_attempt_2_records_correct_stage` —
  start FSM, let attempt 1 fail, slow-mock attempt 2's Builder,
  signal cancel mid-Builder, assert Cancelled event captures
  cancelled_during=Build (NOT Build-attempt-1 — `JobState`
  doesn't track attempt, just current stage).
- `mixed_review_then_test_rejection_uses_one_retry_each` —
  attempt 1: Reviewer rejects → retry. Attempt 2: Reviewer
  approves but Tester rejects → retry. Attempt 3: both approve.
  Final Done, retry_count=2.

#### Integration test

NO real-claude integration test for retry in this WP. Reasoning:
- Real-claude retry would take 5 + 4 + 4 = 13 stages × ~30-60s
  = 6-13 minutes worst-case. Too long to run on every commit.
- The mock-driven tests cover all FSM branches. The substrate
  (real-claude actually responds and parses) is already proven
  by W3-12d's full-chain test.
- A future polish WP can add `integration_retry_real_claude`
  with a deliberately-broken goal that triggers a known
  rejection, but that's W3-12e+ territory.

The orchestrator runs ALL existing integration smokes
(`integration_full_chain_real_claude_with_verdict`,
`integration_cancel_during_real_claude_chain`,
`integration_persistence_survives_real_claude_chain`,
`integration_fsm_drives_real_claude_chain`) post-commit as
regression to confirm the FSM restructure didn't break the
non-retry happy path.

### 8. Bindings

`pnpm gen:bindings` adds the `RetryStarted` variant to the
existing `SwarmJobEvent` discriminated union. No other binding
changes (retry_count was already on Job in W3-12a).

`pnpm gen:bindings:check` exits 0 post-commit.

## Out of scope

- ❌ Increasing MAX_RETRIES beyond 2. The architectural report
  §5.3 fixes this value; raising it is a future tunable.
- ❌ Per-stage retry budget (e.g. "retry Builder up to 3 times
  but Tester only once"). Single budget covers all gates.
- ❌ Adaptive retry (skip retry if Verdict's issues are too
  numerous / too high-severity). Hard-coded MAX_RETRIES.
- ❌ User-facing UI for retry attempt counter / pill. Event
  fires; rendering is a W3-14 follow-up.
- ❌ Resume-from-orphan with retry intent. Orphan recovery
  still finalizes as Failed; user reruns manually via UI.
- ❌ Coordinator LLM brain (W3-12f) deciding "skip retry,
  this issue is unfixable". Routing stays deterministic.
- ❌ Persist a per-attempt audit trail (which Verdict triggered
  which retry). Only the latest verdict is stored. Full audit
  is post-W3.

## Acceptance criteria

- [ ] `MAX_RETRIES=2` is honored: rejections within budget loop
      back to PLAN; rejections beyond exhaust to Failed.
- [ ] `RETRY_PLAN_PROMPT_TEMPLATE` includes goal, scout output,
      verdict issues (prose), previous plan, gate name.
- [ ] Scout runs exactly once per job, regardless of retry count.
- [ ] `SwarmJobEvent::RetryStarted` enum variant added; fires
      once per retry; carries `attempt`, `max_retries`,
      `triggered_by` (JobState), and the rejecting `verdict`.
- [ ] `Job.retry_count` increments on each retry; persists to
      SQLite via existing `update_job` write-through.
- [ ] `Job.last_verdict` always reflects the most recent gate
      verdict (including mid-retry, not just final-fail).
- [ ] All Week-2 + Week-3-prior tests still pass; target ≥285
      passing (272 prior + ≥11 retry-loop tests).
- [ ] No new dep, no new migration, no `unsafe`, no `eprintln!`.
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check`
      exits 0 post-commit.
- [ ] All 4 existing real-claude integration smokes still pass
      (orchestrator-driven post-commit).

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 1 pre-commit
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration regression:
cd src-tauri
cargo test --lib -- integration_full_chain_real_claude_with_verdict --ignored --nocapture
cargo test --lib -- integration_cancel_during_real_claude_chain --ignored --nocapture
```

## Notes / risks

- **Stage indexing in `Job.stages`.** W3-12d's `stages: Vec<StageResult>` was a flat list assuming linear flow. With retries, the same stage state (Plan/Build/Review/Test) appears multiple times. The existing schema stores `(job_id, idx, state)` so duplicates by `state` are fine — `idx` is the global ordinal. UI consumers reading `stages.filter(s => s.state == 'plan')` will see N entries (one per attempt). This is the intended behavior; no schema change needed.
- **Retry_count is the gate, not stages.len().** Don't derive retry count from stages. They tell different stories: stages = how many specialist invocations happened; retry_count = how many full retry-cycles started. A cancel mid-Build at attempt 2 has stages.len()=5+1=6 and retry_count=1.
- **Cost ticker.** Each retry adds ~$0.04-0.10 in claude tokens. Worst case (2 retries × 4 stages each + 1 scout) is ~$0.30-0.50 per job. Document in the AGENT_LOG so the trade-off is visible.
- **Cancel race during retry transition.** If cancel arrives between "RetryStarted fires" and "Plan stage begins," the cancel is queued; next stage's `tokio::select!` catches it. `Cancelled.cancelled_during` will be `Plan` (the next stage), which is correct.
- **`finalize_failed_with_verdict` no longer always called on rejection.** Only called when retry budget exhausted. Tests must distinguish: a single-rejection job that retries successfully must NOT have called `finalize_failed_with_verdict`.
- **Event ordering.** Per retry: `Cancelled`-event-NOT-fired (the loop continues, no terminal); `RetryStarted` fires; then subsequent `StageStarted/Completed` are for the new attempt's stages. UI reasoning: receiving a `RetryStarted` after a `StageCompleted(Review)` with a rejected Verdict implies the FSM is starting attempt N+1.
- **No `Cancelled` event on retry transition.** The job is still running, just looping back. Only `RetryStarted` and the next stage's events fire.

## Sub-agent reminders

- Read this WP in full.
- Read `swarm/coordinator/fsm.rs` (W3-12d shipped) for the existing 5-stage flow.
- Read `swarm/coordinator/verdict.rs` for the Verdict types.
- Read `swarm/coordinator/job.rs` for the SwarmJobEvent enum and `retry_count` field.
- DO NOT add a new dep. The retry loop is plain control flow.
- DO NOT add a new migration. `retry_count` and the columns are all there.
- DO NOT increase MAX_RETRIES — it stays 2.
- DO NOT skip Scout on retries by re-running it — it's idempotent but expensive. Cache the output and reuse.
- DO NOT modify the existing `RETRY_PLAN_PROMPT_TEMPLATE` shape after the WP lands without a follow-up WP — frontends and tests pin its content.
- DO NOT introduce per-attempt sub-stages in JobState. The enum stays the 8-variant set from W3-12d.
- Per AGENTS.md: one WP = one commit.

---
id: WP-W3-12j
title: Coordinator FSM — Fullstack parallel dispatch (Builder ∥ Builder, Reviewer ∥ Reviewer)
owner: TBD
status: not-started
depends-on: [WP-W3-12i]
acceptance-gate: "scope=Fullstack runs `BackendBuilder ∥ FrontendBuilder` concurrently via `tokio::join!`, then `BackendReviewer ∥ FrontendReviewer` concurrently. Both Verdicts must approve to advance to Test. Wall-clock saving ≈ 30-40% on Fullstack jobs vs W3-12i sequential. Cancel mid-parallel propagates to BOTH tracks."
---

## Goal

Parallelize the W3-12i Fullstack sequential chain (BB+BR then
FB+FR) into two concurrent tracks. Per-stage prompt + persona
semantics unchanged; only the FSM's stage scheduling changes.

This is the last sub-WP of the W3-12 swarm series before
W3-12k (Orchestrator chat layer).

## Why now

W3-12i's Fullstack integration smoke proved the 8-stage chain
end-to-end at 743.68s (~12.4 min) on Windows. The two
domain-specific Build+Review pairs are independent — backend
work doesn't depend on frontend output and vice versa — so
sequential execution wastes wall-clock for no correctness gain.

User directive 2026-05-06: "kalite öncelikli, maliyet önemli
değil" — but quality includes UX latency. A 6-9 min Fullstack
job is meaningfully better than a 12-15 min one for the
user-experience curve.

## Charter alignment

No tech-stack change. `tokio::join!` is already used elsewhere
(W3-12c's cancel select is a sibling primitive in the same
crate).

## Scope

### 1. New `dispatch_pair_concurrent` helper

Inside the FSM's `'retry_loop`, replace the sequential for-loop
over `select_chain_pairs(scope)` with parallel dispatch when
the scope yields ≥2 pairs.

```rust
let pairs = select_chain_pairs(decision.scope);
match pairs.len() {
    0 => unreachable!("select_chain_pairs always returns ≥1"),
    1 => {
        // Single-domain: existing W3-12h sequential path
        let (builder_id, reviewer_id) = pairs[0];
        run_pair_then_handle(...).await?;
    }
    _ => {
        // Multi-domain (Fullstack today; Fullstack+ in future):
        // run all pairs concurrently. Each pair independently
        // executes BUILD then REVIEW.
        let pair_futures: Vec<_> = pairs.iter().map(|(builder_id, reviewer_id)| {
            run_pair_concurrent(self, app, builder_id, reviewer_id, /* ... */)
        }).collect();
        let results: Vec<PairOutcome> = futures::future::join_all(pair_futures).await;
        // Aggregate: any rejection → retry; any error → finalize_failed; all approved → continue.
        match aggregate_pair_results(results) {
            AggregateOutcome::AllApproved => { /* fall through to TEST */ }
            AggregateOutcome::SomeRejected(verdicts) => {
                // Use the FIRST rejected Verdict as last_verdict (or
                // synthesize an aggregate verdict listing all rejections).
                if retry_count < MAX_RETRIES { try_start_retry(...); continue 'retry_loop; }
                else { finalize_failed_with_verdict(...); return ...; }
            }
            AggregateOutcome::Errored(err) => {
                finalize_failed(err); return ...;
            }
            AggregateOutcome::Cancelled => return ...;
        }
    }
}
```

`futures::future::join_all` IS a new dep concern — it's part
of the `futures` crate. Check if already in tree (likely as a
transitive dep through tokio's `futures-util`). If not,
hand-roll with `tokio::try_join!` macro for two-pair case
(simpler, no new dep, but harder to extend to N pairs in
future).

### 2. `PairOutcome` enum

```rust
enum PairOutcome {
    /// Both BUILD + REVIEW completed; Verdict approved.
    Approved {
        build: StageResult,
        review: StageResult,
    },
    /// Both BUILD + REVIEW completed; Verdict rejected.
    Rejected {
        build: StageResult,
        review: StageResult,
        verdict: Verdict,
    },
    /// BUILD or REVIEW failed (transport error / parse failure).
    /// `partial` carries any stages that completed before the failure.
    Errored {
        partial: Vec<StageResult>,
        error: AppError,
    },
    /// Cancellation observed.
    Cancelled {
        partial: Vec<StageResult>,
    },
}
```

### 3. `run_pair_concurrent` helper

```rust
async fn run_pair_concurrent<R: Runtime>(
    fsm: &CoordinatorFsm<...>,
    app: &AppHandle<R>,
    builder_id: &str,
    reviewer_id: &str,
    /* shared state: profiles, plan_text, job_id, notify, builder_domain */
) -> PairOutcome {
    // Run BUILD stage.
    let build = match fsm.run_stage_with_cancel(
        app, JobState::Build, builder_profile, &build_prompt, job_id, notify
    ).await {
        StageOutcome::Ok(stage) => stage,
        StageOutcome::Err(e) => return PairOutcome::Errored { partial: vec![], error: e },
        StageOutcome::Cancelled => return PairOutcome::Cancelled { partial: vec![] },
    };
    
    // Run REVIEW stage.
    let review = match fsm.run_verdict_stage(...).await {
        VerdictStageOutcome::Approved(verdict, stage) => return PairOutcome::Approved {
            build, review: stage,
        },
        VerdictStageOutcome::Rejected(verdict, stage) => return PairOutcome::Rejected {
            build, review: stage, verdict,
        },
        VerdictStageOutcome::ParseFailed(stage, err) => return PairOutcome::Errored {
            partial: vec![build, stage], error: err,
        },
        VerdictStageOutcome::InvokeError(err) => return PairOutcome::Errored {
            partial: vec![build], error: err,
        },
        VerdictStageOutcome::Cancelled => return PairOutcome::Cancelled {
            partial: vec![build],
        },
    };
    review
}
```

The PUSH to `Job.stages` happens inside `run_pair_concurrent`
via `self.registry.update`. Ordering across the two parallel
tracks is non-deterministic — Backend BUILD and Frontend BUILD
push to stages whichever finishes first.

This means `Job.stages` for a Fullstack job is no longer
strictly ordered by JobState. Tests that hardcode the
sequential order (`stages[3] == Build_Backend`,
`stages[4] == Review_Backend`, etc.) break. Tests must instead
match on `(state, specialist_id)` pairs across the stages
collection.

### 4. Verdict aggregation

When multiple pairs return Rejected verdicts, the FSM needs to
pick a `last_verdict` to record. Options:

A. **First rejection wins** — `last_verdict` = the first Rejected pair's verdict in iteration order. Simple, deterministic.

B. **Synthesize aggregate verdict** — combine all rejected verdicts' issues into one Verdict with `issues = concat(...)` and `summary = "N pairs rejected: ..."`.

Pick **B** (synthesize aggregate). Reasoning: the user wants to see ALL feedback, not just the first one. The aggregate Verdict reads like a normal Verdict to downstream code (UI render, parse_decision, persistence) but carries a richer issues list.

```rust
fn aggregate_rejections(rejections: Vec<(Verdict, &str /* domain */)>) -> Verdict {
    let mut all_issues = Vec::new();
    for (v, domain) in &rejections {
        for issue in &v.issues {
            // Annotate the issue with its source domain so the user
            // sees "[backend] high: file:42 — message" or
            // "[frontend] med: file:99 — message".
            let mut prefixed = issue.clone();
            prefixed.message = format!("[{domain}] {}", issue.message);
            all_issues.push(prefixed);
        }
    }
    let summary = format!(
        "{n} of {total} parallel pairs rejected; aggregated {issues_count} issues across domains.",
        n = rejections.len(),
        total = pairs.len(),  // captured via closure
        issues_count = all_issues.len(),
    );
    Verdict { approved: false, issues: all_issues, summary }
}
```

### 5. Cancel propagation

Both parallel tracks share the same `Notify` from the FSM's
cancel surface. When `swarm:cancel_job` fires, `notify_one()`
wakes the first track's `tokio::select!`; the second track's
select is also notified (Notify's notify_one wakes ALL waiters
that have called `notified()` since the last notify).

**Verify**: `tokio::sync::Notify::notify_one` semantics — does
it wake one OR all? Per docs: "If there is a waiter, it will be
notified. Otherwise, a permit is stored. The next call to
notified() will then complete immediately."

So `notify_one` wakes ONE task at a time. With two parallel
tracks, only one wakes; the other continues until its own
`tokio::select!` polls `notified()` and immediately receives
(due to stored permit). **This works** — both tracks abort.

But to be safer, use `notify_waiters()` instead of `notify_one()`
when canceling: it explicitly wakes ALL current waiters. The
`signal_cancel` helper in JobRegistry should switch to
`notify_waiters` for parallel-aware cancellation.

### 6. Tests

#### Pure-fn tests
- `aggregate_rejections_concatenates_issues` — 2 rejected verdicts × 3 issues each → 6 issues with domain prefix.
- `aggregate_rejections_summary_format` — assert summary text matches the template.
- `pair_outcome_serde_roundtrip` (if PairOutcome serializes; otherwise skip).

#### FSM tests (mock-driven)

- `fsm_fullstack_parallel_walks_eight_stages_on_approved_path` — both pairs approved; final state Done; stages.len() == 8. Stage ordering tolerates concurrency: assert that the SET of (state, specialist_id) pairs matches the expected set, not their order in `stages[]`.
- `fsm_fullstack_parallel_backend_rejection_retries` — BackendReviewer rejects, FrontendReviewer approves; aggregate verdict has 1 [backend]-prefixed issue; retry kicks in; attempt 2 retries the WHOLE parallel chain.
- `fsm_fullstack_parallel_frontend_rejection_retries` — analogous.
- `fsm_fullstack_parallel_both_rejected_retries` — both Reviewers reject; aggregate verdict has [backend] + [frontend] issues; retry kicks in.
- `fsm_fullstack_parallel_both_rejected_exhausts_retries_finalizes_failed` — both reject on every attempt; finalize Failed with aggregate Verdict.
- `fsm_fullstack_parallel_backend_invoke_error_short_circuits` — BackendBuilder errors; frontend track may or may not have completed; FSM finalizes Failed; partial frontend stages are pushed.
- `fsm_fullstack_parallel_cancel_propagates_to_both_tracks` — start with slow mocks; cancel mid-stage; both tracks abort within 2s.
- `fsm_fullstack_parallel_persistence_round_trip` — pool-backed registry; drive parallel happy path; reload; assert all 8 stages present (order may differ but set matches).
- `fsm_single_domain_unchanged_in_parallel_mode` — scope=Backend (single pair) takes the `pairs.len() == 1` branch, runs sequentially as before; this test confirms 12j didn't accidentally break single-domain.

#### Existing FSM regression

W3-12i's Fullstack tests assumed sequential ordering. Update them to use set-based assertions OR keep them but add explicit `tokio::join!` ordering (deterministic via test-controlled mock delays). Pick the cleaner approach during implementation.

#### Integration test (`#[ignore]`)

`integration_fullstack_parallel_chain_real_claude` — same goal as W3-12i's fullstack test (the imperative "EXECUTE: Edit two source files..." goal), but expect:
- `outcome.final_state == Done`
- `outcome.stages.len() == 8`
- (state, specialist_id) set matches expected
- Wall-clock substantially less than W3-12i's 743s — target < 600s

Re-uses W3-12i's `TestEnvGuard`, isolated CARGO_TARGET_DIR, 600s/stage timeout, imperative goal. Bumped Tester max_turns=24 still applies.

### 7. Bindings

NO wire-shape changes. `PairOutcome` is FSM-internal.
`Job.stages` shape unchanged (still `Vec<StageResult>`); only
the population order is different.

`pnpm gen:bindings:check` exits 0 post-commit.

### 8. UI follow-up note

`SwarmJobDetail.tsx` renders stages in `Job.stages` order.
For Fullstack parallel, ordering is non-deterministic — the
UI might show Build_Frontend before Build_Backend (whichever
finished first). This is acceptable — the user sees the
parallel reality. A future polish could sort stages by
`(domain, state)` for visual stability, but not in this WP.

## Out of scope

- ❌ Cross-domain Verdict signaling (e.g. "BR sees a Frontend issue caused by Backend's API change"). Independent gates remain.
- ❌ Per-domain retry budget (retry only the rejected domain). Current MAX_RETRIES=2 applies to the whole chain.
- ❌ Streaming partial Verdict updates while parallel pairs run. Final Verdict on each pair is what fires the `StageCompleted` event; no mid-stage events.
- ❌ Job-level early-cancel on first rejection (e.g. BR rejects → cancel FB+FR mid-flight). Both pairs run to completion so the user sees both Verdicts. tokio::try_join would change this; we explicitly use join_all (or tokio::join!) to wait for all.
- ❌ Visual sort of stages in UI by domain.
- ❌ Per-pair cost meter on UI.

## Acceptance criteria

- [ ] Fullstack `select_chain_pairs(Fullstack)` two pairs run concurrently via `tokio::join!` (or `futures::join_all` if dep allows).
- [ ] Single-domain (Backend / Frontend) takes the `pairs.len() == 1` branch and runs sequentially (unchanged from W3-12h).
- [ ] `PairOutcome` enum + `aggregate_rejections` helper added.
- [ ] On any-rejection (parallel-aggregate), retry kicks in if budget allows; aggregate Verdict carries domain-prefixed issues.
- [ ] On any-error (parallel), finalize Failed with the error.
- [ ] Cancel mid-parallel propagates to BOTH tracks within 2s (tested via slow-mock + Notify).
- [ ] `Job.stages` for Fullstack contains exactly 8 entries on happy path; ordering is set-based, not sequence-based.
- [ ] Tests assert (state, specialist_id) sets, not strict orders.
- [ ] All Week-2 + Week-3-prior tests pass; target ≥350 passing.
- [ ] No new dep (`futures` crate may be transitively available; if not, hand-roll with `tokio::join!`).
- [ ] `bindings.ts` unchanged.
- [ ] Integration test compiles (orchestrator runs; expect ~6-9 min wall clock).

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 0 expected
pnpm typecheck
pnpm test --run
pnpm lint

# Orchestrator-driven integration smokes:
cd src-tauri
cargo test --lib -- integration_fullstack_parallel_chain_real_claude --ignored --nocapture --test-threads=1
cargo test --lib -- integration_fullstack_chain_real_claude --ignored --nocapture --test-threads=1
```

## Notes / risks

- **Stage ordering in `Job.stages` is now non-deterministic for Fullstack.** Tests, UI, and any downstream consumer must NOT assume order. Documented in the field's doc comment.
- **`notify_waiters()` vs `notify_one()`.** The W3-12c cancel surface uses `notify_one` which is fine for sequential FSM. For parallel, switch to `notify_waiters` so both tracks get the cancel signal. Verified safe — the cancel guard's `Drop` still fires once.
- **Aggregate Verdict size.** With both Reviewers rejecting and each producing 3-5 issues, aggregate could have 6-10 issues. UI render uses `verdict.issues.map(...)` so it scales naturally; persistence is a JSON blob, no row-count concern.
- **Retry replays the parallel chain.** If BR approved and FR rejected, retry re-runs BB+BR (waste) AND FB+FR (the actual problem). Per-domain retry would re-run only FB+FR. Future polish per W3-12i §"Out of scope".
- **Mock test concurrency.** Tokio's mock runtime tolerates `join!` natively; tests using `MockResponse` with `sleep` durations can deterministically order concurrent pair completions. Use this to make order-sensitive tests deterministic when needed.
- **`tokio::join!` vs `futures::future::join_all`.** Macro is for fixed N (we have 2 today). join_all is for `Vec<Future>`. For 2-pair Fullstack, the macro is cleaner. For future N>2 multi-domain (e.g. `Mobile` scope), join_all scales naturally. Sub-agent picks; document choice.

## Sub-agent reminders

- Read this WP in full.
- Read `swarm/coordinator/fsm.rs` (W3-12i state) — particularly the `select_chain_pairs` helper and the W3-12i sequential for-loop body.
- Read `tokio::sync::Notify` docs for the `notify_one` vs `notify_waiters` distinction.
- DO NOT add a new dep. `futures` crate is likely transitively available; verify with `cargo tree | grep ^futures` before relying on `join_all`. If not, use `tokio::join!` macro.
- DO NOT change retry-loop semantics. Aggregate-rejection still triggers a single retry from Plan.
- DO NOT change Coordinator profile. Scope classification is unchanged.
- DO NOT add per-domain retry budget. That's a future polish.
- DO NOT change PairOutcome's persistence — it's runtime-only. The push-to-stages happens inside the run loop; PairOutcome is just the in-memory return shape.
- Pay extra care to **ordering assumptions** in existing W3-12i tests. Update them to set-based assertions OR pin the parallel order via mock delays. Explicit; don't fudge.
- Per AGENTS.md: one WP = one commit.

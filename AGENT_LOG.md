# Agent Log

Running journal of agent-driven changes. Newest entry on top. See `AGENTS.md` § "AGENT_LOG.md" for format.

---

## 2026-05-06T07:18Z WP-W3-14 completed

- dispatch: **single sub-agent**; frontend-only WP, no backend changes, no real-claude integration smoke required (verified Rust regression count unchanged at 254 instead)
- sub-agent: general-purpose
- files changed: 17 in commit `2ace648`
  - new — frontend: `app/src/routes/SwarmRoute.tsx`, `app/src/components/{SwarmGoalForm,SwarmJobList,SwarmJobDetail}.tsx`, `app/src/hooks/{useSwarmJob,useSwarmJobs,useRunSwarmJob,useCancelSwarmJob}.ts`, `app/src/styles/swarm.css`
  - new — tests: `app/src/hooks/{useSwarmJob,useSwarmJobs}.test.tsx`, `app/src/routes/SwarmRoute.test.tsx`, `app/src/components/SwarmJobDetail.test.tsx`
  - new — planning: `docs/work-packages/WP-W3-14-swarm-ui-route.md`
  - modified: `app/src/App.tsx` (+'swarm' route, +NAV/TOPBAR_TITLE entries, +RouteHost case), `app/src/App.test.tsx` (nav-item count 6 → 7), `app/src/main.tsx` (+swarm.css import), `docs/work-packages/WP-W3-overview.md` (W3-12b flipped to done; W3-14 row added)
- commit SHA: `2ace648`
- acceptance: ✅ pass
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → exit 0, **34 passed** (17 prior + 17 new across 5 files)
  - `pnpm lint` → exit 0
  - `cargo check` → exit 0 (regression — no Rust changes)
  - `cargo test --lib` → exit 0, **254 passed; 0 failed; 8 ignored** (regression — unchanged from W3-12b)
  - integration smokes NOT re-run for this WP because backend untouched. The 3-test smoke suite from W3-12b is the most recent green baseline (104.56s + 101.05s + 32.69s on 2026-05-06). Post-commit `pnpm tauri dev` manual UI smoke is owner-driven and out of orchestrator's loop.
- key implementation choices
  - **2-pane layout** — left = goal form + jobs list, right = selected-job detail. Mirrors `RunsRoute.tsx` convention.
  - **TanStack Query + Tauri event subscription** for live updates. `useSwarmJob` calls `commands.swarmGetJob` for the initial load AND `listen<SwarmJobEvent>` for incremental updates; the listener mutates the cache via `qc.setQueryData(applySwarmEventToJobDetail)`. On `finished`, also invalidates `['swarm-jobs']` so the list reflects the terminal state.
  - **`applySwarmEventToJobDetail` is exported as a pure function** so unit tests drive each event-kind branch directly without spinning up the hook. Mirrors the architectural report's §6 reply-matching pattern (events feed a deterministic projection).
  - **Listener cleanup via cancellation flag + `unlisten?.()`** — handles React StrictMode double-invoke safely. Same pattern `usePaneLines.ts` uses.
  - **`workspaceId = "default"` constant.** Multi-workspace UI is post-W3 per WP §"Out of scope".
  - **`useSwarmJobs` polls every 5s** as a backstop in case events miss (window collapsed or initial load); event-driven invalidation is the primary path.
  - **No new icons.** `bot` reused for sidebar Swarm entry (same as Agents — distinguished by label and active route).
  - **No new JS dep.** TanStack Query, React 18, `@tauri-apps/api/event` were all already in tree.
  - **No backend changes.** Bindings shipped by W3-12a/b/c; this WP only consumes them.
- bindings regenerated: no (no Rust changes)
- branch: `main` (local; not pushed; **56 commits ahead of `origin/main`** post-`2ace648`)
- known caveats / followups
  - **Manual UI smoke pending owner verification** post-commit via `pnpm tauri dev`. The Vitest-side hook + component tests cover unit behavior; full window-rendered UX is a human-eyes pass.
  - **No specialist-pane streaming** (the architectural report's §8.2 multi-pane). Single-pane chat-style is the W3-14 contract; multi-pane is a candidate post-W3 polish WP.
  - **No token-level streaming.** Stage-level events only — mid-stage progress shows "running…" with no token-by-token output.
  - **Cancel race during stage-boundary** is handled by W3-12c's backend (cancel during the gap between StageCompleted and next StageStarted is recorded with the *next* stage's state). UI shows the eventual `finished` event's terminal state — no special UI logic needed.
  - **Cost ticker accumulates per-stage** via the live event stream's `stage_completed.stage.totalCostUsd`. Cross-job aggregation (cumulative spend) is post-W3.
- next: WP-W3-12d (REVIEW/TEST states + reviewer/integration-tester profiles + Verdict schema + retry feedback + Coordinator LLM brain Option B). Last leg of the W3-12 swarm series.

---

## 2026-05-06T00:35Z WP-W3-12b completed

- dispatch: **single sub-agent**; orchestrator drove all 3 manual integration smokes per the 2026-05-05 standing directive
- sub-agent: general-purpose
- files changed: 12 in commit `9f8b4de`
  - new: `src-tauri/migrations/0006_swarm_jobs.sql`, `src-tauri/src/swarm/coordinator/store.rs`, `docs/work-packages/WP-W3-12b-sqlite-persistence.md`, `tasks/swarm-phase-2b.md`
  - modified: `src-tauri/src/swarm/coordinator/{job,fsm,mod}.rs` (registry async + JobSummary/JobDetail + recover_orphans + WorkspaceGuard async drop), `src-tauri/src/commands/swarm.rs` (+`swarm_list_jobs` + `swarm_get_job`), `src-tauri/src/lib.rs` (`with_pool` wiring + recover_orphans block_on at startup), `src-tauri/src/db.rs` (migration count 5→6, table count 12→15), `app/src/lib/bindings.ts` (+2 commands +2 types), `docs/work-packages/WP-W3-overview.md` (W3-12c flipped to done)
- commit SHA: `9f8b4de`
- acceptance: ✅ pass
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **254 passed; 0 failed; 8 ignored** (223 prior + 31 new unit; 7 prior ignored + 1 new ignored integration)
  - `pnpm gen:bindings/check/typecheck/test/lint` → all 0 (gen:bindings:check exit 1 pre-commit expected)
  - **orchestrator-driven 3-test integration smoke suite** (Windows + Pro/Max OAuth):
    - `integration_persistence_survives_real_claude_chain` (NEW) → Done in **104.56s** ✅; DB has 1 Done job + 3 stage rows + 0 workspace_lock rows post-completion
    - `integration_fsm_drives_real_claude_chain` (W3-12a regression) → Done in **101.05s** ✅
    - `integration_cancel_during_real_claude_chain` (W3-12c regression) → Cancelled in **32.69s** ✅
- key implementation choices
  - **Write-through, async, inline.** All three mutators (`try_acquire_workspace`, `update`, `release_workspace`) are async and await SQL inline. No fire-and-forget background writer (would race tests, no value gained vs. the 1-3ms WAL-mode write latency).
  - **`JobRegistry::new()` kept for tests** — in-memory only; pool=None. `with_pool(pool)` is the production path. Test plumbing largely unchanged; pool-backed FSM regression tests opt in by constructing `with_pool` instead of `new`.
  - **`sqlx::query` (string-query), not `sqlx::query!` (offline cache)** — per WP constraint. ~12 queries across `store.rs` + `job.rs` use the runtime-checked variants. `.sqlx/` cache untouched (still holds the W2-02 macro entry).
  - **Orphan recovery is destructive of in-flight context.** Non-terminal jobs become `Failed { last_error: "interrupted by app restart" }`; locks released. Cancel-vs-restart distinction lost in the audit trail (both Failed). W3-12d's retry surface (with W3-14 UI) re-runs orphaned goals cleanly.
  - **`WorkspaceGuard::drop` panic-seatbelt** uses `tauri::async_runtime::spawn` to call the now-async `release_workspace` from a sync Drop. Idempotent — happy paths still explicitly await release before returning, so the spawn only fires on panic-unwind.
  - **`JobSummary.goal` char-truncated to 200** at the SQL helper layer (NOT byte-truncated; Turkish multibyte chars stay intact). Truncation at SQL time keeps the wire serialization predictable.
  - **`recover_orphans` runs in `setup` via `block_on`** before `app.manage(registry)`. Mirrors the existing `db::init` pattern. Logs orphan count via `tracing::warn!` if non-zero.
- bindings regenerated: yes (+`swarmListJobs`, +`swarmGetJob`, +`JobSummary`, +`JobDetail`)
- branch: `main` (local; not pushed; **54 commits ahead of `origin/main`** post-`9f8b4de`)
- deviations
  - **Migration table count 12 → 15** (not 14). The WP §"Notes / risks" estimated 14 ("existing 11 + 3 new"), but the actual pre-WP baseline was 12 (counted: agents/edges/mailbox/nodes/pane_lines/panes/runs/runs_spans/server_tools/servers/settings/workflows). Sub-agent surfaced this; orchestrator confirmed via DB introspection. Updated `db.rs::tests::migration_creates_all_expected_tables` to 15.
- known caveats / followups
  - **No resume-after-restart.** Orphan jobs are Failed; W3-12d adds the retry surface that re-runs them.
  - **No pagination beyond 200-row cap.** W3-14 may add cursor-based pagination if recent-jobs panel grows.
  - **No trim policy.** Old jobs accumulate; a separate sweep (parallel to W3-06's OTel trim) is a candidate W3-12b+ commit.
  - **`Job` type still NOT exported in bindings**, but `JobDetail` (the wire-friendly equivalent without bookkeeping fields) IS, so frontend has the types it needs.
- next: WP-W3-14 (React `useSwarmJob` hook + multi-pane UI surface). 12d (Verdict + retry + Coordinator brain) lands after 14 so the retry-from-orphan flow can be eyeballed in the UI.

---

## 2026-05-05T22:15Z WP-W3-12c completed

- dispatch: **single sub-agent** (orchestrator drafted WP + tasks file, sub-agent implemented backend Rust + bindings; orchestrator drove BOTH integration smokes per 2026-05-05 owner directive "terminalden smoke testlerini ayrıca sen yapabiliyorsan eğer onları da senin yapmanı istiyorum")
- sub-agent: general-purpose
- files changed: 11 in commit `3cb6be1`
  - new — planning: `docs/work-packages/WP-W3-12c-streaming-events-cancel.md`, `tasks/swarm-phase-2c.md`
  - modified: `docs/work-packages/WP-W3-overview.md` (W3-12a flipped to done; W3-12c row scope rephrased), `src-tauri/src/events.rs` (+`swarm_job_event(id)` helper), `src-tauri/src/swarm/coordinator/{mod,job,fsm}.rs` (+`SwarmJobEvent` enum, +cancel_notifies map and 3 methods, +`run_job_with_id` test helper, restructured `run_job` with `tokio::select!` per stage, `CancelGuard` Drop seatbelt, `finalize_cancelled`, `emit_swarm_event`), `src-tauri/src/swarm/mod.rs` (re-export), `src-tauri/src/commands/swarm.rs` (+`swarm_cancel_job`), `src-tauri/src/lib.rs` (+command registration, +`SwarmJobEvent` `.typ::<...>()` export), `app/src/lib/bindings.ts` (regen +1 command, +1 union type with 5 kinds)
- commit SHA: `3cb6be1`
- acceptance: ✅ pass — orchestrator independently re-ran every gate after sub-agent return; orchestrator additionally drove BOTH manual integration smokes (W3-12a happy path + W3-12c cancel)
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **223 passed; 0 failed; 7 ignored** (205 prior + 18 new unit; 6 prior ignored + 1 new ignored integration)
  - `pnpm gen:bindings` → exit 0; bindings.ts gained `swarmCancelJob` + `SwarmJobEvent` union
  - `pnpm gen:bindings:check` → exit 1 PRE-COMMIT (expected). POST-`3cb6be1` it exits 0.
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → exit 0 (17/17 frontend tests)
  - `pnpm lint` → exit 0
  - **orchestrator-driven manual integration smokes** (Windows + Pro/Max OAuth):
    - `integration_fsm_drives_real_claude_chain` (W3-12a regression) → Done in **114.57s** ✅
    - `integration_cancel_during_real_claude_chain` (W3-12c) → Failed with `last_error="cancelled by user"` in **41.23s** ✅; `Cancelled` event captured with `cancelled_during` in {Scout, Plan, Build} per the race-tolerant assertion. (Initial transient 0.14s anomaly run was not reproducible; sequential `--test-threads=1` rerun gave the conclusive 41.23s real-claude exercise.)
- key implementation choices
  - **Single per-job event channel with `kind` discriminator** (`swarm:job:{id}:event` payload tagged Started/StageStarted/StageCompleted/Finished/Cancelled). Mirrors W3-06's `runs:{id}:span` precedent. The alternative (5 separate event names) would have forced N listener registrations per job; the discriminator approach uses one.
  - **`tokio::sync::Notify` for cancel** (no new dep). `tokio_util::CancellationToken` would have been idiomatic but pulls a transitive dep; the manual notify pattern is ~3 lines and works identically for our use.
  - **Lock order extended** to `workspace_locks → cancel_notifies → jobs`. The three methods on the new map (`register_cancel`/`unregister_cancel`/`signal_cancel`) each hold only one mutex while running, so they cannot deadlock against existing two-mutex methods.
  - **`CancelGuard` Drop seatbelt** mirrors `WorkspaceGuard` — guarantees `unregister_cancel` fires even on panic / early return inside `run_job_inner`. Belt and braces alongside the explicit unregister at the FSM tail.
  - **`prompt_preview` is char-bounded, not byte-bounded** — Turkish-language profile bodies are multibyte; byte-slicing risks splitting a UTF-8 codepoint and panicking at runtime.
  - **`run_job_with_id` test-only entry point** (`#[cfg(test)]`) lets unit tests pre-register a Tauri event listener before the FSM mints its ULID. Without it, the listener registration races the first event emission and tests would intermittently miss `Started`/first `StageStarted`. Production callers stay on `run_job` which mints its own job_id and forwards to `run_job_inner(None, …)`.
  - **`SwarmJobEvent` `.typ::<...>()` registered explicitly** in `specta_builder_for_export` even though it's not a command return type. Specta only walks types reachable from registered command signatures; without explicit registration `bindings.ts` would have shipped `SwarmJobEvent` as `unknown` to frontend listeners.
  - **Cancel of terminal job → `Conflict`, of unknown job → `NotFound`**. Idempotent re-cancel of an already-cancelled job races the registry observation: the FSM may have already finalized state by the time the second cancel arrives. Test accepts either error kind via `assert!(matches!(...))`.
  - **No frontend code in this WP** beyond `bindings.ts` regen. The React `useSwarmJob` hook + multi-pane subscription UI is W3-14.
- bindings regenerated: yes (+1 command, +1 union type with 5 kinds)
- branch: `main` (local; not pushed; **52 commits ahead of `origin/main`** post-`3cb6be1`)
- known caveats / followups
  - **No DB persistence**. App restart still loses every in-flight job (W3-12b adds SQLite-backed `JobRegistry` on the same trait surface).
  - **No frontend hook**. UI integration (subscribe + cancel-button) lands in W3-14.
  - **No token-level streaming**. Stage-level events only; mid-stage progress is invisible. A future W3-12c+ could extend `SwarmJobEvent` with `AssistantDelta` if owner prioritizes.
  - **Cancel doesn't propagate to subprocess gracefully**. `kill_on_drop(true)` from W3-11 means dropping the future kills the child OS-level. On Windows, this is async; the test asserts "within 2s" rather than synchronous.
  - **Resume after cancel** is a W3-12d concern via the retry surface; cancel always finalizes as Failed in 12c.
- next: WP-W3-12b (SQLite persistence + restart recovery), then WP-W3-12d (REVIEW/TEST states + reviewer/integration-tester profiles + Verdict schema + retry feedback + Coordinator LLM brain), then WP-W3-14 (React UI hook + multi-pane). 12b and 12d are independent of each other; 12d ideally lands after 12b so retry transcripts persist.

---

## 2026-05-05T20:50Z WP-W3-12a completed

- dispatch: **single sub-agent** (W3-11's hybrid cadence retired for this WP — orchestrator drafted the WP + tasks file, sub-agent implemented the entire Rust + bindings surface).
- sub-agent: general-purpose
- files changed: 12 in commit `5890841`
  - new — Rust: `src-tauri/src/swarm/coordinator/{mod,fsm,job}.rs`
  - new — planning: `docs/work-packages/WP-W3-12a-coordinator-fsm-skeleton.md`, `tasks/swarm-phase-2a.md`
  - modified: `docs/work-packages/WP-W3-overview.md` (W3-11 status flipped to done; W3-12a/b/c/d row stubs added; dep graph updated), `src-tauri/src/swarm/{mod,transport}.rs` (`Transport` trait extraction; `SubprocessTransport` impls it; new `MockTransport` under `#[cfg(test)]`), `src-tauri/src/commands/swarm.rs` (+`swarm_run_job` + `stage_timeout()` env-var helper), `src-tauri/src/error.rs` (+`WorkspaceBusy` struct variant; `message()` now returns `Cow<'_, str>`), `src-tauri/src/lib.rs` (`JobRegistry` `app.manage`d; new command registered), `app/src/lib/bindings.ts` (regen +1 command, +3 types: `JobOutcome`, `JobState`, `StageResult`)
- commit SHA: `5890841`
- acceptance: ✅ pass — orchestrator independently re-ran every gate after sub-agent return; OWNER additionally drove the manual integration smoke (3-stage real-claude chain, 120s, `Done`)
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **205 passed; 0 failed; 6 ignored** (181 prior + 24 new = +24 unit; +1 ignored integration)
  - `pnpm gen:bindings` → exit 0; bindings.ts gained `swarmRunJob`, `JobOutcome`, `JobState`, `StageResult`
  - `pnpm gen:bindings:check` → exit 1 PRE-COMMIT (expected). POST-`5890841` it exits 0.
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → exit 0 (17/17 frontend tests)
  - `pnpm lint` → exit 0
  - **owner-driven manual integration smoke** (after two false-start iterations, see "key implementation choices" below): 3-stage chain `scout → planner → backend-builder` against real `claude` binary on Windows + Pro/Max OAuth; canonical goal `"Find the impl ProfileRegistry block in profile.rs and add a one-line public method ... right after the existing list method. Just the method."` → `outcome.final_state == Done` in 120.11s. Builder produced the expected one-line method; reverted from the WP commit (out-of-scope smoke artifact).
- key implementation choices
  - **Pure Rust FSM, no Coordinator LLM** (Option A per architectural report §5.1). Smallest validation surface; trivial upgrade path to Option B (single-shot Coordinator brain) at W3-12d as a 1-2 file refactor.
  - **`async fn in trait` (Rust 1.78+ stable)** — no `async-trait` dep added. `CoordinatorFsm<T: Transport>` is generic over the trait; `SubprocessTransport` and `MockTransport` both impl it. `cargo tree | grep async-trait` confirmed no transitive dep would be free.
  - **Per-workspace lock policy** (owner directive 2026-05-05): same `workspace_id` → second call rejected pre-flight with `AppError::WorkspaceBusy{workspace_id, in_flight_job_id}` (Err side, NOT a Failed-state outcome — pre-flight rejection is a different surface from in-flight stage failure). Different `workspace_id` → parallel. `JobRegistry` holds two mutex-guarded maps; consistent acquisition order (locks → jobs) prevents deadlock. `WorkspaceGuard` Drop impl ensures `release_workspace` fires even on panic.
  - **3-state happy path only** (SCOUT → PLAN → BUILD → DONE). `Review` and `Test` variants exist on `JobState` but are unreachable in 12a; `next_state` `debug_assert!`s on them so a future code change that leaks them surfaces in test builds. W3-12d activates them once `reviewer.md` / `integration-tester.md` profiles + Verdict schema land.
  - **`Job` type NOT exported in bindings**: specta only emits types reachable from registered command signatures. `Job` is internal-only in 12a (no IPC returns it; `JobOutcome` carries the equivalent payload sans bookkeeping fields). Adding a forced export would leak an unused type to the frontend; W3-12c naturally pulls `Job` into the wire surface via a future `swarm:list_jobs` command.
  - **`AppError::message()` signature change**: was `&str`, now `Cow<'_, str>` to synthesize the formatted message for the `WorkspaceBusy` struct variant. Existing variants still hand back `Cow::Borrowed` (zero-cost). All call sites work unchanged via `Cow`'s auto-deref.
  - **Stage-failure record-or-not policy**: chose NOT to push a `StageResult` for the failing stage. Documented in `Job` struct doc-comment and `fsm_*_failure_*` test assertions. `fsm_scout_failure_short_circuits` asserts `stages.is_empty()`.
  - **`render_scout_prompt` content fix** (post-integration): Phase 2a draft specified "scout receives goal verbatim", but real-claude integration test on 2026-05-05 showed Scout burning its 6-turn budget when goal was a "do X" task (Scout's persona forbids writes; verbatim "do X" creates contract conflict). Wrapped goal as investigation: `"Aşağıdaki görev için kod tabanını araştır ... SEN KOD YAZMIYORSUN"`. Manual chain validation from earlier the same day used this exact framing organically — the WP shipped with codified prompt matching that empirical finding. Unit test renamed `prompt_template_scout_passes_goal_verbatim` → `prompt_template_scout_wraps_goal_as_investigation` and updated to assert the investigation framing.
  - **Default per-stage timeout for integration test bumped to 180s** (`NEURON_SWARM_STAGE_TIMEOUT_SEC` override). Production default stays 60s. Reason: Windows + antivirus cold-cache first-spawn of `claude.cmd` can spend 30-60s on AV alone; first attempt at 60s/stage caused a Builder-stage timeout (104.47s, observed 2026-05-05).
- bindings regenerated: yes (+1 command, +3 types)
- branch: `main` (local; not pushed; **50 commits ahead of `origin/main`** post-`5890841`)
- known caveats / followups
  - **No DB persistence**. App restart loses every in-flight job. W3-12b adds SQLite-backed `JobRegistry` on the same trait surface; in-memory impl stays for tests.
  - **No streaming**. `swarm:run_job` blocks the caller for 30-180s. Frontend has no progress UI yet — the W3-12c subscription channel + `useSwarmJob` hook close that gap.
  - **No cancel**. Killing the IPC promise has no effect on the spawned `claude` children mid-job. W3-12c lands cancel propagation alongside streaming.
  - **REVIEW/TEST inert**. Code defines them but they're unreachable; W3-12d activates them.
  - **W3-04 (LangGraph cancel + streaming) still deferred** per Owner decision #4 in `WP-W3-overview.md`; re-evaluate at W3-08 close.
- next: WP-W3-12b (SQLite persistence + restart recovery), or WP-W3-12c (streaming Tauri events + frontend hook + cancel mid-job). 12b/12c can land in either order; 12d depends on at least 12a (this WP) and ideally 12b.

---

## 2026-05-05T18:48Z WP-W3-11 completed

- dispatch: **hybrid** (orchestrator scaffold + Charter, sub-agent parser/transport/tests). First time the `AGENTS.md` "one sub-agent per WP" cadence was split — recorded in `tasks/swarm-phase-1.md` §"Dispatch decision" so future per-WP authors can refer to it.
- sub-agent: general-purpose (Rust code + tests + lib.rs wiring + bindings regen)
- files changed: 19 in commit `f1596f8`
  - new — Rust: `src-tauri/src/swarm/{mod,binding,profile,transport}.rs`, `src-tauri/src/commands/swarm.rs`
  - new — bundled profiles: `src-tauri/src/swarm/agents/{scout,planner,backend-builder}.md` (orchestrator-authored, embedded via `include_dir!`)
  - new — planning: `docs/work-packages/WP-W3-11-swarm-foundation.md`, `tasks/swarm-phase-1.md`
  - modified: `PROJECT_CHARTER.md` (+Swarm runtime row in tech-stack table), `docs/work-packages/WP-W3-overview.md` (+W3-11 status row, +Owner decision #4 documenting Swarm/LangGraph coexist + W3-04 deferral + W3-10 unblock), `src-tauri/Cargo.toml` (+`include_dir = "=0.7.4"`, +`which = "=4.4.2"`), `Cargo.lock`, `src-tauri/src/{lib,error,models,commands/mod}.rs`, `app/src/lib/bindings.ts` (regen +5 entries: `swarmProfilesList`, `swarmTestInvoke`, `ProfileSummary`, `InvokeResult`, `PermissionMode`)
- commit SHA: `f1596f8`
- acceptance: ✅ pass — orchestrator independently re-ran every gate after sub-agent return; OWNER additionally drove the manual integration smoke
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **181 passed; 0 failed; 5 ignored** (163 prior + 18 new = +18)
  - `pnpm gen:bindings` → exit 0; bindings.ts gained 5 typed entries
  - `pnpm gen:bindings:check` → exit 1 PRE-COMMIT (expected; the `git diff --exit-code` guard reports the not-yet-committed regen). POST-`f1596f8` it exits 0.
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → exit 0 (17/17 frontend tests)
  - `pnpm lint` → exit 0
  - **owner-driven manual integration smoke**: `cargo test --manifest-path src-tauri/Cargo.toml --lib -- swarm::transport::tests::integration_smoke_invoke --ignored` → exit 0, real `claude` binary spawned, bundled `scout` profile loaded via `include_dir!`, NDJSON `Say exactly: 'scout-ok' and nothing else.` round-tripped over stream-json, assertion on `result.assistant_text.contains("scout-ok")` passed in **7.59s** on Windows (PowerShell, Pro/Max OAuth)
- key implementation choices
  - **Substrate scope only.** Per WP §"Out of scope": Coordinator state machine, persistent Coordinator chat, multi-pane UI, Verdict schema + JSON parser, retry loop, broadcast/fan-out, MCP per-agent config, DB persistence, streaming, and BYOK transport are all deferred to W3-12+. This WP is the transport + profile loader + smoke command, nothing more.
  - **Persistent vs. per-invoke split** (architectural report §3.3): Coordinator persistence is a W3-12 concern; this WP only ships the per-invoke side via `SubprocessTransport::invoke`. Single Tauri command (`swarm:test_invoke`) returns one `InvokeResult` per call.
  - **Subscription auth preservation**. `subscription_env()` strips `ANTHROPIC_API_KEY` / `USE_BEDROCK` / `USE_VERTEX` / `USE_FOUNDRY` so the spawned `claude` child inherits the user's Pro/Max OAuth token via `~/.claude/` rather than a per-token API path. Defensive `Command::env_remove(...)` calls are layered on top of the cleaned env-map because `envs()` merges into rather than replaces the inherited slate.
  - **`--append-system-prompt-file`, NOT `--system-prompt-file`** (replace mode). The replace flag would erase Claude Code's built-in tool-use behavior (`Read`, `Grep`, etc.); the append form keeps defaults and stacks the persona on top. Asserted in `binding::tests::specialist_args_contain_required_flags`.
  - **`Plan` permission_mode → `--permission-mode plan`, no `--dangerously-skip-permissions`.** Per WP §3 binary gate: Plan-mode profiles (Scout, Planner) cannot trigger writes; non-Plan profiles (BackendBuilder) get `--dangerously-skip-permissions` since the headless smoke command has no UI to surface approval prompts. Asserted in `binding::tests::plan_mode_skips_dangerous_flag`.
  - **Hand-rolled YAML frontmatter parser**. No `gray_matter` / `serde_yaml` dep — the parser is ~50 lines and avoids a transitive `pest`/`yaml-rust` chain. The `id` validation regex `^[a-z][a-z0-9-]{1,40}$` and the 3-part dotted `version` parse are unit-tested.
  - **Three bundled profiles** (per Owner decision 2026-05-05): `scout` + `planner` + `backend-builder`. Even before the W3-12 Coordinator FSM lands, the user can drive a `scout → planner → builder` mini-flow manually by chaining three `swarm:test_invoke` calls — Phase 1 substrate is exercised against multiple personas, not a single one.
  - **Profile dir is `app_data_dir/agents/`** (per Owner decision 2026-05-05), NOT `~/.neuron/agents`. A clean reinstall wipes user-edited profiles together with the rest of the install state — no orphan dotfile survives uninstall.
  - **Cross-runtime hygiene**. `swarm/` never imports from `sidecar/agent.rs` or `agent_runtime/`. LangGraph (scripted "Daily summary" workflow) and Swarm (chat-driven agent-team) share the SQLite store but are otherwise independent runtimes.
  - **`ProfileRegistry::load_from(workspace_dir: Option<&Path>)`** signature — the bundled walk is hardcoded inside the registry via `include_dir!`, not passed as a virtual `&[PathBuf]` entry. Cleaner than the WP §2 draft signature; sub-agent surfaced this in the orchestrator dispatch prompt and the orchestrator approved.
  - **`PermissionMode` parser dual-form**. Accepts both `acceptEdits` (camel) and `accept-edits` (kebab). The bundled `backend-builder.md` ships camel; the WP body used kebab. Tolerating both removes a foot-gun for users authoring workspace overrides. Unit-tested.
- bindings regenerated: yes (+5 typed entries: 2 commands, 3 types)
- branch: `main` (local; not pushed; **48 commits ahead of `origin/main`**)
- known caveats / followups
  - **Charter "Status: Active — Week 2"** is now stale (we are mid-Week-3). Not amended in this WP (out of scope); next planning-housekeeping commit can flip it.
  - **Profile body persona reminders** ("Bu Claude Code'un sıradan davranışı değil — sen Coordinator değil, Specialist'sin") are imperative-style Turkish; the W3-13 era may add an EN parallel for international users. Phase 1 ships TR-only matching the owner's working language.
  - **Tmp file lifecycle**: `app_data_dir/swarm/tmp/<ulid>.md` is deleted on the happy path, preserved on error. No retention policy yet — long-term a sweep removes >24h-old files. Deferred to W3-12.
  - **No DB persistence**: `swarm:test_invoke` is stateless. Migration `0006_swarm_jobs.sql` is reserved for W3-12 once the FSM has somewhere to write (job rows, transcripts, retry history).
  - **W3-04 (LangGraph cancel + streaming) deferred**: per Owner decision #4 in `WP-W3-overview.md`, re-evaluate at W3-08 close. W3-10 (Python embed) is reframed as not-blocked-on-W3-04.
- next: WP-W3-12 (Coordinator state machine + persistent chat + DB persistence + streaming events), or any of the deferred W3 backlog (W3-02 MCP pool, W3-03 MCP install UX, W3-05 approval UI, W3-07 pane aggregates, W3-08 workflow editor, W3-09 capabilities + E2E, W3-10 Python embed). All depend only on already-shipped WPs.

---

## 2026-05-02T01:05Z WP-W3-06 completed

- sub-agent: general-purpose
- files changed: 12 (7 new, 5 modified)
  - new: `src-tauri/src/telemetry/{mod.rs, sampling.rs, otlp.rs, exporter.rs, tests.rs}`, `src-tauri/src/telemetry/tests/fixtures/expected.json`, `src-tauri/migrations/0005_span_export.sql`
  - modified: `src-tauri/Cargo.toml` (+`rand 0.8`, `sha2 0.10`, `reqwest =0.12.23` rustls-tls, `mockito 1` dev), `Cargo.lock`, `src-tauri/src/lib.rs` (`mod telemetry;` + setup hook), `src-tauri/src/sidecar/agent.rs` (`insert_span` writes `sampled_in`), `src-tauri/src/db.rs` (migration count 4 → 5)
- commit SHA: `33e0403`
- acceptance: ✅ pass — orchestrator independently re-ran every gate
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **153 passed, 0 failed, 4 ignored** (135 prior + 18 new)
  - `pnpm gen:bindings:check` → exit 0 (zero diff — no Tauri command added in this WP)
  - `pnpm typecheck`, `pnpm test --run` (17/17), `pnpm lint` → all exit 0
- key implementation choices
  - **No `opentelemetry` SDK dep**: hand-crafted OTLP/JSON v1.3 envelope per WP §3. SDK adoption deferred (the wire format is small and stable; SDK pulls a much larger dep tree).
  - **Deterministic trace/span IDs**: `sha256(run_id)[..16]` and `sha256(span_id)[..8]` hex. Re-exports of the same row produce identical IDs so collectors dedupe by `(traceId, spanId)`. Hash choice locked in a `const`.
  - **4xx sentinel `-1`**: malformed batches flagged with `exported_at = -1` so they cannot block the queue forever. Partial index `WHERE exported_at IS NULL` naturally skips the sentinel.
  - **`reqwest` rustls-tls only**: keeps OpenSSL off the dep tree, relevant for upcoming WP-W3-10 self-contained bundle. Pinned `=0.12.23` exact.
  - **Per-span sampling**: simpler than per-run; per-run sampling deferred (would require tracking decision keyed by `run_id` for the lifetime of the run — sidecar-protocol work).
  - **`gen_ai.prompt` / `gen_ai.completion` truncation @ 1 KiB**: collectors reject oversized attribute strings; truncation prevents whole-batch loss.
  - **`mockito` over `wiremock`**: chosen by sub-agent for simpler async test setup. Each test uses `Server::new_async().await` for parallel-safe isolation.
  - **No new `AppError` variant**: transport errors wrap as `AppError::Internal("OTLP transport: ...")`; HTTP-status errors are Ok-path with `tracing::warn!`. Reuses existing surface.
- bindings regenerated: no (zero diff intended — no Tauri command added)
- branch: `main` (local; not pushed)
- known caveats / followups
  - Endpoint + ratio sourced from env vars in this WP. A small follow-up commit (≤30 lines) wires `settings:get('otel.endpoint')` / `settings:get('otel.sampling.ratio')` into `crate::telemetry` once we want runtime configurability via the Settings UI.
  - In-flight spans (`duration_ms IS NULL`) are NOT exported. WP-W3-04's cancel propagation will need to mark cancelled spans with a `duration_ms` so they can be exported with `status.code = ERROR`.
  - Trim sweep ("delete spans older than N days") is a separate concern, not in this WP.
- next: WP-W3-02 (MCP session pool + cancel safety) or WP-W3-04 (agent runtime cancel + streaming) — both depend only on WP-W3-01 which is done. Author whichever the owner prefers next.

---

## 2026-05-02T00:45Z WP-W3-01 completed (Week 3 kickoff)

- sub-agent: general-purpose
- files changed: 12 (4 new, 8 modified)
  - new: `src-tauri/src/secrets.rs`, `src-tauri/src/commands/secrets.rs`, `src-tauri/src/commands/settings.rs`, `src-tauri/migrations/0004_settings.sql`
  - modified: `src-tauri/Cargo.toml` + `Cargo.lock` (`keyring = "=3.6.3"` per-target deps), `src-tauri/src/lib.rs` (mod + 7 commands), `src-tauri/src/commands/{mod.rs, me.rs}`, `src-tauri/src/db.rs` (test rename + count bump 11→12, migration count 3→4), `src-tauri/src/mcp/registry.rs` (`resolve_env` → `crate::secrets::get_secret`), `app/src/lib/bindings.ts` (regen +28)
- commit SHAs:
  - `621b02c` `chore(lint): wire react-hooks plugin and fix surfaced warnings` — pre-W3-01 lint gate fix (52575ca's eslint-disable directives referenced an unloaded plugin; this commit also fixes two genuine warnings the rule then surfaced in `Canvas.tsx` and `Terminal.tsx` cleanup ref capture)
  - `a351cd2` `feat: WP-W3-01 keychain (Rust) + settings table` — the WP itself
- acceptance: ✅ pass — orchestrator independently re-ran every gate after sub-agent return
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **135 passed, 0 failed, 4 ignored** (110 prior + 25 new = +25)
  - `pnpm gen:bindings` → 7 new commands (`secretsSet/Has/Delete`, `settingsGet/Set/Delete/List`); `secretsGet` deliberately absent
  - `pnpm typecheck`, `pnpm test --run` (17/17), `pnpm lint` → all exit 0 (lint pass requires `621b02c` first)
- key implementation choices
  - **`secrets:get` is NOT a command**: per WP-W3-01 §3 design decision, secret values never cross the IPC boundary. Only `secrets:has` (boolean presence) is exposed. Consumers (`mcp:install`, `runs:create`) read directly via `crate::secrets::get_secret`.
  - **Service name parity with Python**: Rust `keyring::Entry::new("neuron", key)` matches `agent_runtime/secrets.py:SERVICE = "neuron"`. One `secrets:set('anthropic', ...)` write is readable by both Rust MCP runtime and Python agent runtime.
  - **`keyring` per-target deps**: 3.x requires opt-in to a backend feature. Three `[target.'cfg(...)'.dependencies]` blocks (Windows / macOS / Linux) so each platform pulls only its native backend. Pinned to `=3.6.3`.
  - **Generic API**: per the 2026-05-01 owner decision, no Rust enum or const list of provider names. The `crate::secrets::*` API is generic over `key: &str` so future providers (`gemini`/`groq`/`together`) land as Settings-UI dropdown changes, not API changes.
  - **Settings table is `WITHOUT ROWID`** — small key/value table; saves a btree level on lookup.
  - **Dot-namespaced keys**: `user.name`, `workspace.name`, future `otel.endpoint`, `theme.mode`. The namespace becomes a fixed enum once W3-09 narrows capabilities; for now the column is plain TEXT.
- bindings regenerated: yes (+28 lines, 7 new commands)
- branch: `main` (local; not pushed; 2 new commits on top of `a8866de`)
- known caveats / followups
  - Tauri capability for `secrets:*` and `settings:*` rides on tauri-specta's invoke handler; no `capabilities/default.json` change in this WP. Explicit allowlist enumeration is W3-09.
  - `settings:list` returns specta-tuple wire shape `[string, string][]`. If the W3-09 Settings UI prefers `{key, value}[]`, that's a one-line model refactor.
  - W3-06 (telemetry export, parallel-authored in `a8866de`) is unblocked and ready for sub-agent dispatch.
- next: WP-W3-06 (telemetry export — OTLP/JSON sweep + insert-time sampling)

---

## 2026-04-30T18:32:54Z WP-W2-08 prep + 4-agent followup completed
- sub-agents: B (mcp catalog), C (me:get), A (panes domain), D (operasyonel hygiene) — dispatched in 4 parallel terminals per `tasks/agent-briefs-2026-04-29.md`
- commits: `7596386` (pre-package), `52b270f` (4-agent package), `e1a813c` (bindings regen)
- new files (across the 3 commits):
  - sub-agent additions: `src-tauri/src/tuning.rs`, `src-tauri/src/commands/util.rs`, `src-tauri/src/commands/me.rs`, `src-tauri/migrations/0003_panes_approval.sql`, 6 MCP manifests (`linear/notion/stripe/sentry/figma/memory.json`), `tasks/agent-briefs-2026-04-29.md`
  - pre-package additions (bug-fix + refactor + contract amendments): `docs/adr/0007-id-strategy.md`, `docs/adr/0008-sidecar-ipc-framing.md`, `src-tauri/migrations/0002_constraints.sql`, `src-tauri/src/events.rs`, `src-tauri/src/time.rs`, `tasks/refactor-v1.md`, `tasks/report-29-04-26.md`, `tasks/todo.md`
- modifications: `PROJECT_CHARTER.md` (+Constraints #1 carve-out, #8 timestamp, #9 id), `docs/adr/0006-…md` (`.` → `:` separator amendment), `models.rs` (Mailbox `from`/`to` rename per Charter #1, Pane 5 new fields, `ApprovalBanner` + `Me`/`User`/`Workspace` types), `Neuron Design/app/data.js` (s1-s12 → slug realign), `lib.rs` (`mod tuning`/`util`, subscriber init, `commands::me::me_get` registration), `db.rs` / `sidecar/{agent,terminal}.rs` / `mcp/client.rs` (`eprintln!` → `tracing::*`, constants → `crate::tuning::*`), `commands/runs.rs` (rollback inline → `commands::util::finalise_run_with`), `commands/terminal.rs` (Pane SELECT genişle + status-guarded approval blob parse), `commands/mailbox.rs` (validation messages aligned to wire `from`/`to`), `Cargo.toml` (+`tracing`, +`tracing-subscriber`), regen `app/src/lib/bindings.ts`
- new commands: `me:get`
- mcp catalog: 6 → 12 servers (Linear, Notion, Stripe, Sentry, Figma, Memory added as catalog-only stubs)
- tracing adopted, all active `eprintln!` (test/bin scope hariç) migrated
- acceptance: ✅ pass — orchestrator independently re-ran the gates after every sub-agent return + after each commit
  - `cargo test --lib` → exit 0, **102 passed, 3 ignored** (95 prior + 2 me + 3 panes + 2 util)
  - `cargo check --tests` → exit 0 (4 unrelated `unused_mut` warnings on `mcp/client.rs:570/572`)
  - `cargo run --bin export-bindings` → bindings.ts regenerated (+120/-13)
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → 1 file 2 tests passed
  - `pnpm lint --max-warnings=0` → exit 0
- key implementation choices (this round)
  - **Charter Constraint #1 carve-out**: display-derived strings (`started: "2 min ago"`, `uptime: "12m 04s"`) ship as raw `_at`/`_ms` fields; frontend hook layer derives the human form. Single bounded carve-out — structural fields remain non-negotiable.
  - **MailboxEntry wire revert**: `fromPane`/`toPane` → `from`/`to` with `#[serde(rename)]`; Rust fields keep `_pane` for SQL column binding. ADR-0006 separator promoted from `.` to `:` to match Tauri 2.10 reality.
  - **ApprovalBanner persistence**: `panes.last_approval_json TEXT` (migration 0003); reader-side regex extraction with placeholder fallback; `terminal_list` parses **only when** `status = 'awaiting_approval'`.
  - **MCP catalog stub pattern**: 6 new catalog-only manifests (`spawn: null`); `mcp:install` against them surfaces `McpServerSpawnFailed` cleanly. `installed: true|false` mock flag mismatch deferred to Week 3 G2.
  - **`tracing` over `eprintln!`**: setup hook initialises `tracing_subscriber::fmt().with_env_filter(…).try_init()` (panic-safe for tests). `RUST_LOG=neuron=debug` honored.
  - **File-level staging**: pre-package and 4-agent diffs were physically interleaved in modified source files (models.rs, lib.rs, db.rs, sidecar/*, mcp/*, commands/{mod,runs,terminal}.rs). Atomic 5-commit split would have required hunk-level staging; A2-modified 3-commit split shipped instead. Commit messages disclose the constraint.
- bindings regenerated: yes (`Pane` 5 fields, `ApprovalBanner`, `Me`/`User`/`Workspace`, `commands.meGet`)
- branch: `main` (local; not pushed; **3 new commits on top of `7dba715`**)
- next: WP-W2-07 (span/trace persistence — completes WP-04 event chain; depends only on WP-04) or WP-W2-08 (frontend mock→real wiring — biggest WP, 7 routes + cleanup; now unblocked since pre-package + 4-agent closed all known wire-shape gaps)

---

## 2026-04-29T12:50:37Z WP-W2-06 completed
- sub-agent: general-purpose
- files changed: 8 in commit `351c234`
  - new: `src-tauri/src/sidecar/terminal.rs` (TerminalRegistry, ring buffer, regex status detection, CSI stripping, agent-kind inference)
  - modified: `src-tauri/src/commands/terminal.rs` (replaced WP-W2-03 stubs; added `terminalWrite`, `terminalResize`, `terminalLines`), `src-tauri/src/lib.rs` (registry wiring + `RunEvent::ExitRequested` shutdown hook), `src-tauri/src/models.rs` (`PaneSpawnInput` confirmed, `PaneLine` added), `src-tauri/src/sidecar/mod.rs` (`pub mod terminal`), `src-tauri/Cargo.toml` (+`portable-pty`, +`regex`), `Cargo.lock`, `app/src/lib/bindings.ts` (regenerated)
- commit SHA: `351c234`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **86 passed, 3 ignored** (75 prior + 11 new terminal tests; 2 prior + 1 new opt-in shell-spawn integration)
  - new tests verify: ring buffer overflow drops oldest 1,000, CSI stripper preserves text + removes cursor controls, awaiting-approval regex matches Claude/Codex/Gemini canonical prompts, agent-kind inference from cmd, default shell resolution per platform, registry concurrency (no shared mutable state across panes), kill-pane is idempotent for already-dead children, ring-buffer flush on close populates `pane_lines`, since_seq cursor reads from DB after pane close, resize zero-dim rejection, unknown-pane 404
  - `cargo check` → exit 0
  - `cargo run --bin export-bindings` → bindings.ts regenerated with `terminalWrite`, `terminalResize`, `terminalLines` typed wrappers
  - frontend regression: `pnpm typecheck/lint/test --run` all green (1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / migrations / db.rs / mcp / sidecar/agent.rs / other-command files touched
- key implementation choices
  - **Event name**: `panes:{id}:line` payload `{ k, text, seq }` (`:` separator per ADR-0006 carryover; matches WP-04's `runs:{id}:span` and WP-05's `mcp:installed/uninstalled`).
  - **Reader runtime**: `tokio::task::spawn_blocking` because `portable-pty` exposes `std::io::Read` (sync). CRLF normalised to LF for storage; CSI sequences stripped before persisting to `pane_lines`; raw text preserved in live event payload for xterm.js rendering in WP-W2-08.
  - **Master+writer drop on child exit**: required for Windows ConPTY (the reader pipe is a clone independent of the master Arc). Without dropping, the blocking `read()` never unblocks.
  - **Default shell resolution** (Windows): `pwsh.exe` if `where.exe pwsh.exe` succeeds, else `powershell.exe`. Resolved at spawn time, not cached.
  - **Agent-kind inference** from cmd substring: `claude-code`, `codex`, `gemini`, default `shell`. Persisted in `panes.agent_kind`.
  - **Ring buffer**: 5,000 lines per pane in memory; on overflow drop oldest 1,000; on child exit OR `kill_pane`, flush remaining ring lines to `pane_lines` table for hydration after restart.
  - **Status state machine**: `idle → starting → running → (awaiting_approval ↔ running) → success | error`; awaiting transitions driven by per-agent regex set on the last 5 lines.
  - **Idempotent kill**: tolerates Win32 `ERROR_INVALID_PARAMETER (87)` and Unix `ESRCH` so killing a child that exited mid-flight returns Ok.
- bindings regenerated: yes (3 new typed wrappers + `PaneLine` struct)
- branch: `main` (local; not pushed; **20 commits ahead of `origin/main`**)
- next: WP-W2-07 (span/trace persistence — completes the WP-04 event chain) or WP-W2-08 (frontend mock→real wiring — biggest WP, 7 routes + cleanup)

---

## 2026-04-29T11:36:15Z WP-W2-05 completed
- sub-agent: general-purpose
- files changed: 17 in commit `1ffa084`
  - new module: `src-tauri/src/mcp/{mod,client,registry,manifests}.rs`
  - new manifests: `src-tauri/src/mcp/manifests/{filesystem,github,postgres,browser,slack,vector-db}.json` (6 servers)
  - new doc: `src-tauri/MCP.md` (spec version pin `2024-11-05` + `npx` runtime requirement)
  - modified: `src-tauri/src/commands/mcp.rs` (replaced WP-W2-03 stubs; added `mcpListTools`, `mcpCallTool`), `src-tauri/src/db.rs` (added `seed_mcp_servers`), `src-tauri/src/{error,lib,models}.rs`, `app/src/lib/bindings.ts` (regenerated)
- commit SHA: `1ffa084`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **75 passed, 2 ignored** (56 prior + 19 new MCP tests; 1 prior `#[ignore]`d + 1 new `integration_filesystem_install_and_call` opt-in)
  - new tests verify: NDJSON frame round-trip, registry CRUD, seed idempotency, persist-across-pool-reopen, list ordering (featured first), uninstall flow, install + tools/list integration against real `@modelcontextprotocol/server-filesystem`
  - `cargo check` → exit 0
  - 19 unit tests + 1 ignored npx integration test pass
  - `cargo run --bin export-bindings` → bindings.ts regenerated with `mcpListTools`, `mcpCallTool`, `Tool`, `ToolContent`, `CallToolResult` typed wrappers
  - frontend regression: `pnpm typecheck/lint/test --run` all green (1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / migrations / sidecar / other-command files touched
- key implementation choices
  - **Wire format**: NDJSON over stdio (one UTF-8 JSON object per line, `\n`-terminated) per MCP spec — different from WP-W2-04's length-prefixed framing.
  - **`argsJson: string`** on `mcpCallTool` IPC (not `serde_json::Value`): specta produces broken TS for arbitrary JSON values, so callers `JSON.stringify(args)`. Pragma documented in `commands/mcp.rs`.
  - **No migration file**: seed is data-dependent on `manifests/*.json`, so `db::seed_mcp_servers` runs from `db::init` after migrations (parallels WP-W2-04's `seed_demo_workflow`). Idempotent via `INSERT OR IGNORE`; user-toggled `installed` flag never overwritten on re-seed.
  - **Filesystem server fully wired**: install → spawn `npx -y @modelcontextprotocol/server-filesystem <path>` → `tools/list` → persist `server_tools` rows. Other 5 seeded servers (github, postgres, browser, slack, vector-db) surface `mcp_server_spawn_failed` if the user tries to install them — Week 3 wires the full pipeline. The `mcp:list` returns all 6 with metadata regardless.
  - **No session pool**: each `mcp:callTool` re-spawns the server. Pooling deferred to Week 3 alongside sandbox isolation.
  - **MCP spec version pinned** to `2024-11-05` in MCP.md (Charter risk register's "spec churn" mitigation).
  - **Event names**: `mcp:installed` / `mcp:uninstalled` (`:` separator per ADR-0006 carryover; matches WP-W2-03's mailbox precedent).
- bindings regenerated: yes (new typed wrappers for the 2 new commands + 3 new types)
- branch: `main` (local; not pushed; **17 commits ahead of `origin/main`**)
- next: WP-W2-06 (terminal sidecar) or WP-W2-07 (tracing — depends on WP-W2-04, also unblocked)

---

## 2026-04-28T23:33:29Z WP-W2-04 completed
- sub-agent: general-purpose
- files changed: 23 in commit `5d390e4`
  - new: `src-tauri/sidecar/agent_runtime/` (Python project: pyproject.toml, uv.lock, .python-version, README, .gitignore, `agent_runtime/{__init__,__main__,framing,secrets}.py`, `agent_runtime/workflows/{__init__,daily_summary}.py`, `agent_runtime/tests/{test_framing,test_daily_summary}.py`)
  - new: `src-tauri/src/sidecar/{mod.rs, agent.rs, framing.rs}`
  - modified: `Cargo.lock`, `src-tauri/Cargo.toml` (tokio +process,+io-util features), `src-tauri/src/{lib.rs, commands/runs.rs, error.rs}`, `app/src/lib/bindings.ts` (regenerated, 9-line diff in `runsCreate` docstring; signature unchanged)
- commit SHA: `5d390e4`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **56 passing, 1 ignored** (47 prior + 9 new sidecar tests; the ignored test is the live-Python integration `integration_spawn_then_shutdown_kills_child`, opt-in)
  - python tests (sub-agent ran via `uv run pytest` in sidecar dir): 13 passing (7 framing round-trip + 6 daily_summary including `no_api_key` path)
  - `cargo check` → exit 0
  - `runs:create` now dispatches to sidecar when `SidecarHandle` is in `app.try_state`; happy-path test asserts run row with `status='running'` and zero spans
  - `RunEvent::ExitRequested` hook calls `SidecarHandle::shutdown()`; `kill_on_drop(true)` is the seatbelt
  - no_api_key path emits structured span `attrs.error='no_api_key'`, run ends with `status='error'` (asserted by `test_no_api_key_path_emits_error_span_and_ends_in_error`)
  - frontend regression: `pnpm typecheck/lint/test --run` all green (still 1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / design-spec / terminal-report / migrations files touched
- key implementation choices
  - **Event naming**: emits `runs:{id}:span` with a `kind: "created"|"updated"|"closed"` discriminator (NOT three event names). Stays consistent with the WP-W2-03 `:` substitution forced by Tauri 2.10's `IllegalEventName` panic on `.`.
  - **Stdio framing**: 4-byte big-endian u32 length + UTF-8 JSON body, 16 MiB cap, symmetric on both sides. Codec round-trip-tested on Python and Rust independently.
  - **LangGraph pin**: `>=0.2,<0.3` per WP §"Notes / risks".
  - **Python pin**: `.python-version → 3.11` (uv installed Python 3.11.15 in `.venv`); host's 3.14 left out because LangGraph 0.2.x compatibility on 3.14 is unproven.
  - **API keys**: `keyring.get_password('neuron', 'anthropic')` per Charter §"Hard constraints" #2; never logged.
  - **Span emission**: explicit from each LangGraph node, NOT via LangChain ChatModel callbacks (per WP §"Sub-agent reminders").
  - **Mock tool nodes**: `fetch_docs`/`search_web` return canned strings; real MCP tools land in WP-W2-05.
- bindings regenerated: yes (9-line diff, docstring-only on `runsCreate`)
- branch: `main` (local; not pushed; **13 commits ahead of origin/main**)
- next: WP-W2-05 (MCP registry), WP-W2-06 (terminal sidecar), or WP-W2-07 (tracing — depends on WP-W2-04). Three options, all unblocked by this WP.

---

## 2026-04-28T22:40:30Z WP-W2-03 completed
- sub-agent: general-purpose (initial pass rate-limited mid-execution; orchestrator-led fix-up pass landed on a fresh general-purpose sub-agent invocation)
- files changed: 22 in commit `35c4a85`
  - new: `src-tauri/src/{models.rs, error.rs, test_support.rs, bin/export-bindings.rs}`, `src-tauri/src/commands/{agents,workflows,runs,mcp,terminal,mailbox}.rs`, `src-tauri/test-manifest.{rc,xml}`, `app/src/lib/bindings.ts` (302 lines, generated)
  - modified: `Cargo.lock`, `pnpm-lock.yaml`, `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/commands/{mod.rs, health.rs}`, `app/package.json`, `app/eslint.config.js`
- commit SHA: `35c4a85`
- acceptance: ✅ pass — orchestrator independently re-ran all gates after sub-agent return
  - `cargo check` → exit 0
  - `cargo test --manifest-path src-tauri/Cargo.toml` → exit 0, **47/47 tests passing** (5 db + 39 command + 3 error tests)
  - 17 commands compiled and registered: agents (5: list/get/create/update/delete), workflows (2: list/get), runs (4: list/get/create/cancel), mcp (3: list/install/uninstall), terminal (3: list/spawn/kill), mailbox (2: list/emit) — plus existing `health_db` smoke
  - `app/src/lib/bindings.ts` generated by `cargo run --bin export-bindings`; tauri-specta provides typed JS wrappers (`commands.agentsList()`)
  - `pnpm typecheck` → exit 0 (after adding `@tauri-apps/api ^2.10.1` to `app/package.json`)
  - `pnpm lint` → exit 0 (`app/src/lib/bindings.ts` added to `app/eslint.config.js` ignores; tauri-specta emits one unavoidable `any` cast)
  - `mailbox:new` event fires after `mailbox:emit` succeeds (verified by `mailbox::tests::mailbox_emit_fires_mailbox_new_event`)
  - AppError shape `{ kind, message }` verified by per-namespace error-path tests (e.g. `agents_get_unknown_id_is_not_found`, `runs_cancel_already_done_is_conflict`)
  - Stub commands return only documented side effects (`runs:create` inserts `status='running'` row with no spans; `mcp:install` flips `installed=1`; `terminal:spawn` inserts `status='idle'` pane row)
  - frontend regression: `pnpm test --run` → 1 file 2 tests still passing
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / design-spec / terminal-report files touched
- deviations from WP-W2-03 strict file allowlist (orchestrator-authorized):
  - `app/package.json`: +`@tauri-apps/api ^2.10.1` (required for `bindings.ts` to import `__TAURI_INVOKE`; without it `pnpm typecheck` fails)
  - `app/eslint.config.js`: `src/lib/bindings.ts` added to `ignores` (generated file, single unavoidable `any`)
  - `src-tauri/src/bin/export-bindings.rs`: orchestrator pre-applied `CARGO_MANIFEST_DIR` path anchor to fix relative-path bug that wrote `bindings.ts` to `Desktop/app/...` outside the workspace
  - `src-tauri/build.rs` modified + `src-tauri/test-manifest.{rc,xml}` added: Common-Controls v6 application manifest required for cargo lib-test exes on Windows. `tauri-runtime-wry` imports `TaskDialogIndirect` from comctl32 v6; without a manifest the test binary fails at startup with `STATUS_ENTRYPOINT_NOT_FOUND` (0xC0000139). Fix: disable `tauri-build`'s default manifest, compile own via `rc.exe` in `build.rs`, emit unscoped `cargo:rustc-link-arg=` so production + test exes share one manifest section
- **⚠️ ADR-0006 divergence — needs follow-up**: ADR-0006 specifies event names of shape `{domain}.{id?}.{verb}` with `.` as separator (e.g. `mailbox.new`, `runs.{id}.span`). Tauri 2.10's event-name validator rejects `.` and panics with `IllegalEventName`. Code uses `:` substitution: `mailbox:new`, `agents:changed`, `mcp:installed`, `mcp:uninstalled`. Future WP-W2-06 (`panes:{id}:line`) and WP-W2-07 (`runs:{id}:span`) will follow the same `:` pattern. The shape `{domain}{sep}{id?}{sep}{verb}` is preserved with `:` instead of `.`. **ADR-0006 should be amended in a small follow-up commit** to either (a) record the `.` → `:` substitution, or (b) document that `.` works (if a future Tauri version relaxes the validator).
- IPC naming reality: Tauri's `#[command]` macro forbids `:` in Rust identifiers; the IPC wire uses underscore form (`agents_list`). The colon-namespace ergonomics specified by Charter live in tauri-specta's typed JS wrappers (`commands.agentsList()` etc.) consumed via `import { commands } from './lib/bindings'` in WP-W2-08.
- WP-W2-02 carryover resolved: `health_db` is registered alongside the 17 new commands; tauri-specta exposes it as `commands.healthDb()` on the JS side.
- `.bridgespace/` directory (user's IDE hook artifact) is untracked and intentionally excluded from this commit. Add to `.gitignore` in a separate small commit if desired.
- branch: `main` (local; not pushed; 9 commits ahead of `origin/main`)
- next: WP-W2-04 (LangGraph agent runtime), WP-W2-05 (MCP registry), or WP-W2-06 (terminal sidecar) — all three depend only on WP-W2-03

---

## 2026-04-28T19:27:40Z WP-W2-02 completed
- sub-agent: general-purpose
- files changed: 8 (`src-tauri/Cargo.toml`, `src-tauri/migrations/0001_init.sql`, `src-tauri/src/db.rs` (new module, 244 lines incl. 5 tests), `src-tauri/src/lib.rs` (setup hook + manage pool + register health_db), `src-tauri/src/commands/mod.rs` (new), `src-tauri/src/commands/health.rs` (new, smoke command), `src-tauri/.sqlx/query-976b52de…json` (offline cache), `Cargo.lock`)
- commit SHA: `8870de6`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test --manifest-path src-tauri/Cargo.toml -- db` → exit 0, **5/5 tests passing**:
    - `migration_creates_all_eleven_tables` — list matches expected sorted set
    - `pragma_foreign_keys_is_on_per_connection` — verified across 3 connections
    - `migrations_are_idempotent` — second-launch + fresh-pool, exactly 1 row in `_sqlx_migrations`
    - `pool_can_insert_and_select` — round-trip via the agents table
    - `macro_query_uses_offline_cache` — `sqlx::query_scalar!` compiles + runs against `.sqlx/`
  - `cargo check` → exit 0, 0.70s warm
  - 11 schema tables present in `0001_init.sql`: agents, edges, mailbox, nodes, pane_lines, panes, runs, runs_spans, server_tools, servers, workflows
  - `.sqlx/` offline cache committed (1 query JSON for the test macro)
  - DbPool wired via `app.manage(pool)` in `lib.rs` setup hook; smoke command `health_db` returns `{ tables, foreignKeysOn }`
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `app/` / `docs/` files touched
  - frontend regression check: `pnpm typecheck` ✅, `pnpm lint` ✅, `pnpm test --run` ✅ (still 1 file 2 tests — Hello Neuron + OKLCH)
  - manual `pnpm tauri dev` + `sqlite3 .tables` verification: pending — sandbox cannot launch desktop window
- naming deviation (transparent): smoke command exposed as `health_db` (underscore) instead of charter-canonical `health:db` (colon). Reason: Tauri 2.x's `#[tauri::command]` does not ship a stable `rename = "..."` attribute without extra crates; per WP-W2-02 explicit allowance the underscore form is acceptable for this WP only. WP-W2-03 introduces `tauri-specta` binding generation which will alias the IPC surface back to colon-namespaced names.
- informational: actual Tauri bundle identifier is `app.neuron.desktop` (set in WP-W2-01's `tauri.conf.json`) — DB file lands at `%APPDATA%\app.neuron.desktop\neuron.db` on Windows, NOT the WP body's example `com.neuron.dev`. WP body comment was illustrative; behaviour follows the actual identifier.
- toolchain: `sqlx-cli` installed via `cargo install sqlx-cli --no-default-features --features sqlite` (one-time, on user PATH; not a project dependency)
- branch: `main` (local; not pushed)
- next: WP-W2-03 (Tauri command surface) — depends on WP-W2-02 only

---

## 2026-04-28T18:26:30Z WP-W2-01 completed
- sub-agent: general-purpose
- files changed: 19 (key: `app/{package.json,vite.config.ts,vitest.config.ts,index.html,tsconfig*.json,eslint.config.js}`, `app/src/{main.tsx,App.tsx,App.test.tsx,styles.css,test/setup.ts,vite-env.d.ts}`, `src-tauri/{Cargo.toml,build.rs,tauri.conf.json,src/{main.rs,lib.rs},capabilities/default.json,icons/}`, root `{package.json,pnpm-workspace.yaml,Cargo.toml,Cargo.lock,pnpm-lock.yaml,.nvmrc,.gitignore,.cargo/config.toml}`)
- commit SHA: `d0bbffa`
- acceptance: ✅ pass — orchestrator independently re-ran all 4 non-interactive gates after sub-agent return
  - `pnpm typecheck` → exit 0 (`tsc -b --noEmit`)
  - `pnpm lint` → exit 0 (`eslint --max-warnings=0`)
  - `pnpm test --run` → exit 0 (1 file, 2 tests: "Hello Neuron" render + `--background` OKLCH token assertion)
  - `cargo check --manifest-path src-tauri/Cargo.toml` → exit 0 (0.60s on warm cache)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` or `neuron-docs/` files touched
  - manual `pnpm tauri dev` window-open verification: pending — sandbox cannot open desktop window; user must verify
- deviation from sub-agent file allowlist: `.cargo/config.toml` added (out-of-allowlist). Reason: this Windows host has a partial MSVC + KitsRoot10 registry mismatch causing `cargo check` to fail with `LNK1181: oldnames.lib / legacy_stdio_definitions.lib` despite both libs existing in alternate directories. The config.toml adds project-local `/LIBPATH` rustflags using 8.3 short paths so cargo can compile Tauri's Win32 dependency tree end-to-end. Sub-agent disclosed transparently in its report; orchestrator accepts the deviation as project-local, Charter-compatible (no new tech, no global state mutation), and necessary to reach the WP's `cargo check exits 0` acceptance gate on this host.
- toolchain bootstrap performed by sub-agent: `pnpm@10.33.2` via `npm i -g`, Rust `1.95.0 stable` via `rustup-init` (minimal profile). Both placed `cargo`/`pnpm` on user PATH.
- branch: `main` (local; not pushed)
- next: WP-W2-02 (SQLite schema + migrations) — depends on this WP only

---

## 2026-04-28T17:30:54Z docs/review-2026-04-28 completed
- sub-agent: orchestrator-direct (manual route per SUBAGENT-PROMPT § "Notes for the orchestrator" — docs-only pass, sub-agent delegation overhead skipped)
- files changed: 4 (1 added: `docs/adr/0006-event-naming-and-mailbox-realtime.md`; 3 modified: `docs/work-packages/WP-W2-01-tauri-scaffold.md`, `docs/work-packages/WP-W2-03-command-surface.md`, `docs/work-packages/WP-W2-08-frontend-wiring.md`)
- commits (in order): `8d61b75`, `9b24047`, `8024b5d`
- acceptance: ✅ pass — 3 commits in correct order, 4 files diff against `main`, working tree clean, all `Co-Authored-By` trailers present, no files outside `docs/` touched
- branch: `docs/review-2026-04-28` (local; not pushed)
- next: orchestrator awaits user confirmation to merge `docs/review-2026-04-28` → `main` and proceed to WP-W2-01 delegation

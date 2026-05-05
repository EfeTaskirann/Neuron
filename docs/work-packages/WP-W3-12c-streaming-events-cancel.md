---
id: WP-W3-12c
title: Coordinator FSM — streaming Tauri events + cancel mid-job
owner: TBD
status: not-started
depends-on: [WP-W3-12a]
acceptance-gate: "`swarm:run_job` emits `swarm:job:{id}:event` payloads at every state transition + stage start/complete; `swarm:cancel_job(job_id)` kills the in-flight stage's `claude` subprocess and finalizes the job as `Failed` with `last_error = 'cancelled'`. A DevTools subscriber sees the full event stream during a real-claude job; cancel during BUILD aborts within 2s."
---

## Goal

Land the streaming + cancel surface that turns Phase 2a's
blocking `swarm:run_job` into a UX-friendly long-running
operation. Frontend (in W3-14) will subscribe to per-job event
streams and offer a "Cancel" button.

This WP is **backend + bindings only** — no React hook, no
multi-pane UI. Those land in W3-14 once we have a workflow editor
to attach them to.

## Why now / scope justification

W3-12a packages the 3-stage chain into a single `swarm:run_job`
IPC, but the caller waits 30-180s with zero visibility and no
escape hatch. Two concrete user-facing problems this WP closes:

1. **No "running…" indicator.** A blocking IPC for 30-180s with
   no progress hint is hostile UX. Stage-level events (state
   transition + stage start + stage complete) are enough to
   render a "currently in BUILD…" pill.
2. **No cancel.** A user who realizes mid-Build that the goal
   was wrong has to wait 60-180s. `swarm:cancel_job` gives them
   an out within 2s.

Token-level streaming (assistant message deltas) is **out of
scope** — it requires a deeper Transport refactor and Phase 2c's
goal is to make blocking go away, not to perfect mid-stage
visibility. Token streaming can land later as W3-12c+ if owner
prioritizes it.

## Charter alignment

No new tech-stack row. Tauri events use the existing `:` separator
per ADR-0006 + WP-W2-03 reality. No new IPC pattern; per-id event
names follow the precedent set by `runs:{id}:span` (W3-06) and
`panes:{id}:line` (W2-06).

## Scope

### 1. Event surface (Rust → frontend)

Single per-job event channel `swarm:job:{job_id}:event` with a
`kind` discriminator (mirrors `runs:{id}:span` pattern from
W3-06). Payload shape:

```rust
#[derive(Debug, Clone, Serialize, specta::Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmJobEvent {
    /// Fires once at FSM start, after workspace lock acquired,
    /// before any stage spawns. `state` is `Init`.
    Started {
        job_id: String,
        workspace_id: String,
        goal: String,
        created_at_ms: i64,
    },
    /// Fires before every stage spawns its `claude` subprocess.
    /// `state` is the upcoming stage (Scout / Plan / Build).
    StageStarted {
        job_id: String,
        state: JobState,
        specialist_id: String,
        prompt_preview: String,   // first 200 chars of the rendered prompt
    },
    /// Fires after a stage's `StageResult` is recorded.
    StageCompleted {
        job_id: String,
        stage: StageResult,
    },
    /// Fires once at the FSM tail, regardless of outcome.
    /// `state` is `Done` or `Failed`.
    Finished {
        job_id: String,
        outcome: JobOutcome,
    },
    /// Fires when `swarm:cancel_job` signals — the stage future
    /// races a CancellationNotify; the stage's subprocess is
    /// dropped (kill_on_drop) and the FSM finalizes as Failed
    /// with `last_error = "cancelled by user"`.
    Cancelled {
        job_id: String,
        cancelled_during: JobState,   // which stage was running
    },
}
```

The discriminator makes it one event name, easy to subscribe to.
Rationale: matches W3-06's `runs:{id}:span` pattern (`kind` union
`"created"|"updated"|"closed"`); the alternative (4 separate
event names) costs more boilerplate and forces frontend to
register N listeners per job.

`prompt_preview` (200 chars) is included so a debug UI can show
"Builder is working on: …" without leaking the full prompt to
the wire. Full prompts stay server-side.

`Finished.outcome` carries the same `JobOutcome` that
`swarm:run_job`'s blocking return delivers — frontend can
either subscribe and ignore the IPC return, or use the IPC
return and skip the events. Both surfaces stay in lock-step.

### 2. New Tauri command `swarm:cancel_job`

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_cancel_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<(), AppError>;
```

Semantics:
- Look up `job_id` in `JobRegistry`.
- If absent → `Err(AppError::NotFound("swarm job ...".into()))`.
- If terminal (`Done`/`Failed`) → `Err(AppError::Conflict("...".into()))`. Idempotent cancel of an already-cancelled job returns Conflict; the W3-12d retry surface will surface this as expected.
- If in-flight → fire the cancellation signal, return `Ok(())` immediately. Actual abort happens in the FSM's stage loop; observers see the `Cancelled` event then `Finished`.

The cancel signal mechanism: `JobRegistry` gains a
`HashMap<job_id, Arc<tokio::sync::Notify>>` for in-flight
notifies. `swarm:cancel_job` calls `notify.notify_one()`; the
FSM's stage loop selects between the stage future and
`notify.notified()`. The notify is registered on
`run_job` start, removed on Finished/Cancelled.

`tokio::sync::Notify` is already in the `tokio` dep (no new
dep). Per W3-12a's "no new deps without justification" rule,
`tokio_util::CancellationToken` is NOT used.

### 3. FSM changes (`coordinator/fsm.rs`)

`run_job` is restructured:

```rust
pub async fn run_job<R: Runtime>(
    &self,
    app: &AppHandle<R>,
    workspace_id: String,
    goal: String,
) -> Result<JobOutcome, AppError> {
    // ... validation, workspace_id check, build Job, register notify ...
    let notify = Arc::new(Notify::new());
    self.registry.register_cancel(&job.id, Arc::clone(&notify))?;
    let _guard = WorkspaceGuard::new(&self.registry, workspace_id.clone(), job.id.clone());

    emit(app, &job.id, SwarmJobEvent::Started { ... });

    // For each stage:
    emit(app, &job.id, SwarmJobEvent::StageStarted { ... });
    let stage_result = tokio::select! {
        result = self.run_stage(app, state, profile, prompt, &job.id) => result,
        _ = notify.notified() => {
            // Cancel arrived during this stage.
            emit(app, &job.id, SwarmJobEvent::Cancelled { job_id, cancelled_during: state });
            return self.finalize_cancelled(&job.id, &workspace_id, state);
        }
    };
    emit(app, &job.id, SwarmJobEvent::StageCompleted { ... });
    // ... record stage, advance ...

    // Tail:
    let outcome = self.build_outcome(&job.id)?;
    emit(app, &job.id, SwarmJobEvent::Finished { job_id, outcome: outcome.clone() });
    self.registry.unregister_cancel(&job.id);   // also done by Drop guard as belt+braces
    Ok(outcome)
}
```

`emit()` is a thin helper that wraps `app.emit(&format!("swarm:job:{job_id}:event"), &payload)` + a `tracing::debug!` log line. Errors from `emit` are swallowed with a warn — the FSM never aborts because a Tauri event failed to dispatch (e.g. window closing).

`finalize_cancelled` mirrors `finalize_failed` but stamps
`Job.last_error = Some("cancelled by user")` and
`Job.state = Failed` with a different `tracing::warn!` reason.
Cancel is a flavor of failure.

### 4. `JobRegistry` changes

Add a `cancel_notifies: Mutex<HashMap<String, Arc<Notify>>>` map.
Methods:

- `register_cancel(&self, job_id: &str, notify: Arc<Notify>) -> Result<(), AppError>` — insert; duplicate is `AppError::Conflict`.
- `unregister_cancel(&self, job_id: &str)` — idempotent remove.
- `signal_cancel(&self, job_id: &str) -> Result<(), AppError>` — looks up, calls `notify_one()`. Absent or already-removed → `NotFound`.

Lock acquisition order extended: `workspace_locks → cancel_notifies → jobs`. All three are independent mutexes; consistent ordering keeps deadlock-free.

### 5. Tauri command surface

Adds `swarm_cancel_job` registration in
`lib.rs::specta_builder_for_export` under the existing `// swarm`
block.

`swarm:run_job` keeps its existing IPC signature unchanged —
the events fire as a side channel. Callers can ignore them.

### 6. Tests

Mock-driven (no real `claude`):

- `events_started_fires_first` — MockTransport with scripted Ok responses; subscribe to the event channel via Tauri's mock `Listener`; assert ordered: `Started → StageStarted(Scout) → StageCompleted(Scout) → StageStarted(Plan) → StageCompleted(Plan) → StageStarted(Build) → StageCompleted(Build) → Finished`.
- `events_finished_carries_outcome` — `Finished` event's `outcome` deeply equals the IPC return value.
- `events_stage_failure_skips_subsequent_stages` — mock errors on Plan; events are `Started → StageStarted(Scout) → StageCompleted(Scout) → StageStarted(Plan) → Finished` with no `StageCompleted(Plan)`.
- `cancel_during_scout_fires_cancelled_then_finished` — start FSM, wait for `StageStarted(Scout)` event, signal cancel via `swarm:cancel_job`, assert `Cancelled { cancelled_during: Scout }` then `Finished` arrive.
- `cancel_during_plan_fires_cancelled_with_plan_state`.
- `cancel_during_build_fires_cancelled_with_build_state`.
- `cancel_unknown_job_id_returns_not_found`.
- `cancel_already_terminal_returns_conflict`.
- `cancel_double_signal_is_idempotent_after_first` — second cancel during the same job returns Conflict (job already cancelled by then; or `NotFound` if the cancel completed quickly enough). Test accepts either error kind.
- `register_cancel_duplicate_returns_conflict`.
- `unregister_cancel_is_idempotent`.

Integration test (`#[ignore]`):
- `integration_cancel_during_real_claude_chain` — spawn the real-claude FSM with the canonical goal, wait for `StageStarted(Build)` event, signal cancel, assert `Cancelled` and `Finished` arrive within 5s, assert `outcome.final_state == Failed`, `outcome.last_error == Some("cancelled by user")`. CI-skipped same as W3-11/12a integration tests.

The existing `integration_fsm_drives_real_claude_chain` from W3-12a stays — keeps the happy-path proof.

Target test delta: ≥10 unit + 1 ignored integration. New baseline ≥215 unit.

### 7. Bindings regen

`pnpm gen:bindings` adds:
- `commands.swarmCancelJob(jobId) -> Promise<void>`
- New types: `SwarmJobEvent` union with all 5 kinds.

`pnpm gen:bindings:check` exits 0 post-commit.

## Out of scope

- ❌ React hook (`useSwarmJob`) — W3-14 (UI multi-pane WP)
- ❌ Token-level streaming (assistant message deltas) — defer; current scope is stage-level events
- ❌ Per-stage cancel granularity (cancel only the current stage, continue to next) — cancel always finalizes the job
- ❌ Resume after cancel — not in 12c. W3-12d's retry loop will need a different surface for this
- ❌ Persistence of cancel state across app restarts — W3-12b
- ❌ Multi-window event routing — `app.emit` broadcasts to all windows; per-window filtering is post-W3-13
- ❌ Backpressure on event listener slowness — Tauri's event bus drops slow listeners by default; we don't override

## Acceptance criteria

- [ ] `SwarmJobEvent` enum defined and `specta::Type`'d
- [ ] FSM emits `Started → StageStarted/Completed × 3 → Finished` on a happy-path job
- [ ] FSM emits `Started → StageStarted → … → Finished` (no `StageCompleted`) on a stage-error path
- [ ] `swarm:cancel_job(job_id)` Tauri command compiles, types end-to-end
- [ ] Cancel during any stage results in `Cancelled` then `Finished` events; subprocess killed within 2s of cancel signal (kill_on_drop)
- [ ] Cancel of unknown job_id → `Err(AppError::NotFound)`
- [ ] Cancel of terminal job → `Err(AppError::Conflict)`
- [ ] `JobRegistry::{register,unregister,signal}_cancel` honor lock ordering, no deadlock under concurrent stress test
- [ ] No new `unsafe`, no new dep, no `eprintln!`
- [ ] All Week-2 + Week-3-prior tests still pass (regression: 205 + new tests; target ≥215)
- [ ] Integration test (`#[ignore]`d) compiles
- [ ] `bindings.ts` regenerated with `swarmCancelJob` + `SwarmJobEvent`

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exits 1 pre-commit, 0 post
pnpm typecheck
pnpm test --run
pnpm lint

# Owner-driven (or orchestrator-driven per 2026-05-05 directive):
cd src-tauri
cargo test --lib -- integration_cancel_during_real_claude_chain --ignored --nocapture
cargo test --lib -- integration_fsm_drives_real_claude_chain --ignored --nocapture
```

## Notes / risks

- **`app.emit` failure modes**. Tauri's emit can fail if the window is closing during a stage transition. The FSM swallows the error with a `tracing::warn!`; the IPC return value is still authoritative. Test: emit failure during `Finished` → `swarm:run_job` IPC still resolves with the correct `JobOutcome`.
- **Race between cancel signal and stage completion**. If a stage finishes microseconds before the cancel arrives, the FSM has already moved to the next stage's `StageStarted`. The next stage's `select!` then catches the cancel and emits `Cancelled { cancelled_during: <next_state> }`. Test: cancel fired immediately after `StageCompleted(Plan)` → `Cancelled { cancelled_during: Build }`. This is the documented behavior, not a bug.
- **Subprocess kill latency**. `kill_on_drop(true)` (W3-11) means dropping the future kills the subprocess. On Windows, OS-level kill is async; the test asserts "within 2s" rather than synchronous. Realistic worst case: 200-500ms.
- **Event-payload size**. `Finished.outcome` carries the full `JobOutcome` including all `StageResult.assistant_text`s — could be 100KB+ for code-heavy stages. Tauri's IPC envelope handles this; just noting it. If a future WP wants thin events, a `kind: "finished_summary"` variant with metadata-only could be added without removing `Finished`.
- **Workspace lock + cancel timing**. The `WorkspaceGuard` Drop releases the lock; cancel doesn't bypass that. Two cancellations of the same workspace's job in quick succession both honor the lock — the second `swarm:run_job` for that workspace_id post-cancel succeeds because the previous job's Drop guard already ran.

## Sub-agent reminders

- Read this WP in full before writing code. The `SwarmJobEvent` shape is the contract.
- Read `src-tauri/src/swarm/coordinator/fsm.rs` (W3-12a) for the existing FSM structure. Do not rewrite the stage loop from scratch — extend the existing one with `tokio::select!` and event emits.
- Read `src-tauri/src/sidecar/agent.rs` for the existing Tauri-event emit pattern (`runs:{id}:span` with `kind` discriminator) — mirror it.
- DO NOT add `tokio_util` or `tokio-util` deps. `tokio::sync::Notify` is already in tree.
- DO NOT add token-level streaming. Stage-level events only.
- DO NOT change `swarm:run_job`'s IPC return shape. Events are a side channel.
- DO NOT change `swarm:test_invoke` semantics.
- DO NOT introduce frontend code beyond `bindings.ts` regen. No `useSwarmJob` hook in this WP.
- Per `AGENTS.md`: one WP = one commit. Orchestrator handles atomicity.

---
id: WP-W5-05
title: Cancel + workspace serialization under the message-bus
owner: Claude Opus 4.7 (1M context)
status: implemented
depends-on: [WP-W5-03]
acceptance-gate: "`swarm:cancel_job` IPC migrated to emit `MailboxEvent::JobCancel` (instead of signaling the FSM's per-job `Notify` directly). `MailboxBus::emit_typed` gains a workspace-busy guard for `JobStarted` events that returns `AppError::WorkspaceBusy` when another brain-driven job is in-flight for the same workspace. `RunEvent::ExitRequested` shutdown hook emits `JobCancel` for every in-flight brain-driven job. The W3 FSM cancel path stays untouched for v1 jobs. `cargo test --lib` ≥ 8 new unit tests; `pnpm typecheck` / `lint` / `gen:bindings:check` green."
---

## Result

Branch: `wp-w5-05-cancel-workspace-lock`. Implementation per the contract:

- `MailboxBus::emit_typed` body grew a workspace-busy guard at
  the top of the function: when the inbound event is
  `MailboxEvent::JobStarted`, the bus refuses with
  `AppError::WorkspaceBusy` if another brain-driven, non-terminal
  `swarm_jobs` row exists for the same workspace. The current
  job's id is excluded so the v2 IPC's up-front
  `try_acquire_workspace` write does not self-trip. FSM-source
  rows are ignored.
- `swarm:cancel_job` IPC discriminates on `swarm_jobs.source`:
  brain → emit `MailboxEvent::JobCancel`; fsm or no DB row →
  legacy `JobRegistry::signal_cancel` (W3-12c semantics
  preserved verbatim); unknown string → `AppError::Internal`.
  IPC signature unchanged — the bus + pool come from
  `app.state` lookups inside the function body.
- `RunEvent::ExitRequested` hook calls
  `MailboxBus::cancel_in_flight_brain_jobs` BEFORE
  `agent_registry.shutdown_all()` so dispatchers can break out
  of their `tokio::select!`'s and finish in-flight `claude`
  turns instead of getting SIGKILL'd mid-stream. The fan-out
  body lives on `MailboxBus` so the shutdown invariant is
  unit-testable without booting the runtime closure.
- Test gates green:
  - `cargo build --lib` exit 0
  - `cargo test --lib` **525 passed / 0 failed / 15 ignored**
    (baseline 516 + 9 new — 8 contract-listed + 1 sister
    `emit_typed_ignores_fsm_source_jobs_for_busy_check` pinning
    the FSM/brain coexistence path called out in §"Notes / risks")
  - `cargo check --all-targets` exit 0
  - `pnpm gen:bindings` regen — only docstring updates land in
    `app/src/lib/bindings.ts` (no public type changes)
  - `pnpm gen:bindings:check` exit 0 (post-commit)
  - `pnpm typecheck` / `pnpm lint` exit 0
  - `pnpm test --run` 64 passed / 1 pre-existing locale flake
    (matches W5-04 baseline)

### Caveats

- The WP example SQL (`SELECT COUNT(*) FROM swarm_jobs WHERE
  workspace_id = ? AND source = 'brain' AND state NOT IN ('done',
  'failed')`) implicitly assumed the projector inserts the
  swarm_jobs row AFTER `emit_typed`. In the current code path the
  v2 IPC's `try_acquire_workspace` writes the row up-front, so
  the guard would self-trip. The implementation excludes the
  JobStarted's own job_id (`AND id != ?`) and switched
  `COUNT(*)` to a one-row `SELECT id ... LIMIT 1` so the same
  query returns the in-flight job_id for the
  `AppError::WorkspaceBusy { workspace_id, in_flight_job_id }`
  variant the existing `AppError` enum requires.
- `RunEvent::ExitRequested` already runs inside a
  `block_on` chain. Adding the bus cancel fan-out at the head
  of the swarm cleanup order matches the WP's
  "BEFORE agent_registry.shutdown_all()" directive verbatim.

## Goal

Migrate cancel signaling and workspace serialization from
in-process Notify + HashMap (the W3-12a/12c pattern) to
mailbox-driven equivalents, so the brain (W5-03) and dispatchers
(W5-02) can subscribe to cancel signals without the FSM in the
loop.

This is parallel-runable with W5-04 — both depend only on W5-03's
event surface.

## Why now

Owner directive 2026-05-09 §1: relax the FSM. W5-05 specifically
removes the FSM's per-job `Notify` map as the cancel substrate
for v2 jobs; the brain + dispatchers subscribe to bus events
instead. Without this WP, `swarm:cancel_job` against a v2 job
would hang because the FSM's notify map has no entry for it.

## Charter alignment

- **Tech stack**: no new dependency.
- **Frontend mock shape**: `swarm:cancel_job` IPC signature
  unchanged. The wire shape is preserved; only the backend
  implementation switches between FSM-style cancel and
  mailbox-style cancel based on the job's `source` column
  (W5-04).
- **Identifier strategy / timestamp invariant**: N/A.

## Scope

### 1. `MailboxBus::emit_typed` workspace-busy guard

Extend the bus's emit path to enforce one in-flight brain-driven
job per workspace. The check fires only for `JobStarted` events:

```rust
pub async fn emit_typed<R: Runtime>(
    &self,
    app: &AppHandle<R>,
    workspace_id: &str,
    /* ... */
    event: MailboxEvent,
) -> Result<MailboxEnvelope, AppError> {
    if let MailboxEvent::JobStarted { .. } = &event {
        // Query swarm_jobs for any non-terminal brain-driven row
        // for this workspace.
        let busy_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM swarm_jobs \
             WHERE workspace_id = ? AND source = 'brain' \
               AND state NOT IN ('done', 'failed')",
        )
        .bind(workspace_id)
        .fetch_one(&self.pool)
        .await?;
        if busy_count > 0 {
            return Err(AppError::WorkspaceBusy);
        }
    }
    /* ... existing emit path ... */
}
```

The check uses the `swarm_jobs.source` column from W5-04. FSM-
driven jobs (`source='fsm'`) are tracked by the existing
`JobRegistry::workspace_locks` HashMap; brain-driven jobs use
this SQL-based check. Both substrates honor the "one job per
workspace" rule jointly.

### 2. `swarm:cancel_job` migration

`commands/swarm.rs::swarm_cancel_job` switches on the job's
`source`:

```rust
pub async fn swarm_cancel_job<R: Runtime>(
    /* ... */
    job_id: String,
) -> Result<(), AppError> {
    // Look up the job's source.
    let source: Option<String> = sqlx::query_scalar(
        "SELECT source FROM swarm_jobs WHERE id = ?"
    ).bind(&job_id).fetch_optional(pool.inner()).await?;
    match source.as_deref() {
        Some("brain") => {
            // W5-05 path: emit JobCancel to the bus. The brain
            // task + active dispatchers subscribe and unwind.
            let workspace_id: String = sqlx::query_scalar(
                "SELECT workspace_id FROM swarm_jobs WHERE id = ?"
            ).bind(&job_id).fetch_one(pool.inner()).await?;
            bus.emit_typed(
                &app, &workspace_id,
                "agent:user", "agent:coordinator",
                "cancel",
                None,
                MailboxEvent::JobCancel { job_id: job_id.clone() },
            ).await?;
            Ok(())
        }
        Some("fsm") | None => {
            // W3 path: signal the FSM's per-job notify directly.
            job_registry.signal_cancel(&job_id)
        }
        Some(other) => Err(AppError::Internal(
            format!("unknown swarm_jobs.source: {other}")
        )),
    }
}
```

### 3. Workspace shutdown cancel-fan-out

`lib.rs::run`'s `RunEvent::ExitRequested` hook gets one new step:
before shutting down agent sessions, emit `JobCancel` for every
non-terminal brain-driven job in every workspace:

```rust
if let Some(bus) = app.try_state::<Arc<crate::swarm::MailboxBus>>() {
    let bus = bus.inner().clone();
    let pool_for_cancel = /* ... */;
    tauri::async_runtime::block_on(async move {
        // Find every in-flight brain-driven job and emit JobCancel.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, workspace_id FROM swarm_jobs \
             WHERE source='brain' AND state NOT IN ('done','failed')"
        )
        .fetch_all(&pool_for_cancel)
        .await
        .unwrap_or_default();
        for (job_id, ws) in rows {
            let _ = bus.emit_typed(
                &app_handle,
                &ws,
                "agent:user",
                "agent:coordinator",
                "shutdown",
                None,
                MailboxEvent::JobCancel { job_id },
            ).await;
        }
    });
}
```

This runs BEFORE `agent_registry.shutdown_all()` so dispatchers
have a chance to unwind cleanly before sessions die.

### 4. Brain task cancel handling

`CoordinatorBrain::run` (W5-03) already handles `JobCancel` in
its event loop. W5-05 doesn't change that path; the wire-up is
already in W5-03's scope.

### 5. Tests

- Bus-level workspace-busy guard:
  - emit_typed_rejects_concurrent_job_started_for_same_workspace
  - emit_typed_allows_job_started_after_previous_finished
  - emit_typed_allows_concurrent_job_started_for_different_workspaces

- Cancel IPC dispatch:
  - cancel_job_brain_source_emits_job_cancel_event
  - cancel_job_fsm_source_signals_notify
  - cancel_job_unknown_source_returns_internal_error
  - cancel_job_nonexistent_id_returns_not_found

- Shutdown cancel fan-out:
  - shutdown_emits_job_cancel_for_each_in_flight_brain_job

## Out of scope

- ❌ Migrating FSM cancel to bus events — FSM stays in W5-05; the
  cancel IPC switches based on source. W5-06 deletes the FSM
  cancel path along with the rest of the FSM.
- ❌ Multi-workspace cancel fan-out from a single IPC call —
  IPC takes one job_id, emits one JobCancel. Multi-workspace
  bulk cancel is post-W5.
- ❌ User-driven "cancel all jobs in workspace" — possible follow-up
  polish.

## Acceptance criteria

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` ≥ baseline + 8
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regen + commit (no new commands; just
      regenerate to be safe)
- [ ] `pnpm gen:bindings:check` exits 0
- [ ] `pnpm typecheck` / `pnpm lint` / `pnpm test --run` exit 0
- [ ] All listed unit tests exist + pass

## Files allowed to modify

- `src-tauri/src/swarm/mailbox_bus.rs` (workspace-busy guard in
  `emit_typed`)
- `src-tauri/src/commands/swarm.rs` (`swarm_cancel_job` source-
  switching)
- `src-tauri/src/lib.rs` (RunEvent::ExitRequested cancel fan-out)
- `app/src/lib/bindings.ts` (regen)
- `docs/work-packages/WP-W5-05-cancel-workspace-lock.md`
- `AGENT_LOG.md`

MUST NOT touch:
- FSM (`swarm/coordinator/fsm.rs`)
- Brain (`swarm/brain.rs` from W5-03)
- Projector (`swarm/projector.rs` from W5-04)
- Mailbox bus typed-IPC surface (W5-01 — only the emit path
  changes, not the IPC contract)
- Frontend components (cancel button stays wired to the same IPC)

## Notes / risks

- **Race between IPC return and JobCancel propagation**:
  `swarm:cancel_job` emits the JobCancel event and returns
  immediately. The brain + dispatchers process it on their next
  event loop iteration. Total latency ≤ 100ms in practice. No
  guarantee of synchronous cancel — same as today's FSM cancel
  path.
- **Workspace-busy guard SQL cost**: one COUNT query per
  JobStarted emit. Acceptable; emits are rare (few per job).
  If profiling shows hot-path overhead, cache an in-memory
  busy-set keyed on workspace.
- **Cancel during help round-trip**: a specialist might be
  awaiting a CoordinatorHelpOutcome via the bus when JobCancel
  arrives. The dispatcher's tokio::select! breaks out; the
  specialist's session is left mid-turn. The next job's first
  dispatch to the same agent triggers a respawn (per W4-02
  crashed-session semantics). Documented inline.
- **FSM cancel + brain cancel coexistence**: both paths can fire
  concurrently for different jobs. The source-switching in
  cancel_job IPC routes correctly; the workspace-busy guard
  prevents two brain jobs in one workspace; the JobRegistry's
  workspace_locks prevents two FSM jobs in one workspace; but
  one brain + one FSM job in the same workspace is *technically
  possible* until W5-06 deletes the FSM. Mitigation: the bus's
  workspace-busy check + the existing FSM check are orthogonal,
  so emitting JobStarted for workspace W when an FSM job is
  already running doesn't fail. We accept this transient
  inconsistency through W5-05; W5-06 closes it by removing the
  FSM path entirely.

## Result

(Filled in by the sub-agent on completion.)

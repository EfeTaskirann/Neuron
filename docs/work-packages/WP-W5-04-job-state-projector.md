---
id: WP-W5-04
title: Job state derived from mailbox + UI plumbing (`JobProjector` synthesises `SwarmJobEvent` stream)
owner: TBD
status: not-started
depends-on: [WP-W5-03]
acceptance-gate: "New `JobProjector` service subscribes to the workspace `MailboxBus`, synthesises `SwarmJobEvent`s on the existing `swarm:job:{id}:event` Tauri channel, and persists job rows under a new `swarm_jobs.source='brain'` flag. `swarm:run_job_v2` returns a fully-shaped `JobOutcome` derived from the event log. Frontend hooks (`useSwarmJob`, `useSwarmJobs`, `useRunSwarmJob`) work unchanged against brain-driven jobs. `cargo test --lib` ≥ 15 new unit tests; `pnpm typecheck` / `lint` / `gen:bindings:check` green."
---

## Goal

Make brain-driven jobs (W5-03) appear in the existing UI surface
(3×3 grid + chat panel + recent jobs list + job inspector) without
any frontend changes. The wire shape stays the same:

- Job-level streaming events flow on `swarm:job:{job_id}:event`
  with payload `SwarmJobEvent`.
- Persistent rows live in `swarm_jobs` / `swarm_stages`.
- `swarm:get_job` / `swarm:list_jobs` return the same `Job` /
  `JobSummary` / `JobDetail` shapes.

What changes is the *source* — instead of the FSM emitting
SwarmJobEvents directly, a per-workspace `JobProjector` task
listens to the bus and synthesises the same events from primitive
mailbox events.

## Why now

Owner directive 2026-05-09 §1: preserve the UI investment while
relaxing the FSM. W5-03 ships brain-driven dispatch but emits
primitive bus events; W5-04 maps those to the existing job-event
contract so the 3×3 grid + chat panel + recent jobs list keep
working.

W5-04 is also the gate before W5-06 (FSM teardown) — until the
projector is solid, deleting the FSM would break the UI.

## Charter alignment

- **Frontend mock shape**: preserved verbatim. Charter Constraint
  #1 honored — backend produces the same `SwarmJobEvent` /
  `Job` / `JobOutcome` / `StageResult` shapes the FSM emits today.
- **Identifier strategy** (ADR-0007): brain-driven jobs use the
  same `j-<ULID>` form. No new identifier domain.
- **Timestamp invariant** (Charter §8): all `_at` / `_ms` field
  semantics preserved.
- **Tech stack**: no new dependency.

## Scope

### 1. Migration `0011_swarm_jobs_source.sql`

```sql
-- 0011 — swarm_jobs source discriminator (WP-W5-04).
--
-- Adds one nullable column to distinguish FSM-driven jobs (W3)
-- from brain-driven jobs (W5-03):
--
--   * `source` — 'fsm' for jobs run via swarm:run_job (the W3
--     FSM path), 'brain' for jobs run via swarm:run_job_v2 (the
--     W5-03 mailbox-driven path). Backfill is 'fsm' for every
--     existing row (the migration runs on databases that pre-
--     date W5).
--
-- ALTER TABLE on SQLite is restricted; ADD COLUMN with a default
-- backfills cleanly.

ALTER TABLE swarm_jobs ADD COLUMN source TEXT NOT NULL DEFAULT 'fsm';
```

`Job` struct gains an `Option<String>` `source` field with serde
default `"fsm"` so older persisted JSON deserialises unchanged.

### 2. New module `src-tauri/src/swarm/projector.rs`

```rust
//! `JobProjector` — mailbox → SwarmJobEvent + swarm_jobs row
//! synthesiser (WP-W5-04).
//!
//! One projector task per workspace. Subscribes to the
//! `MailboxBus`, walks each `MailboxEnvelope` through a
//! state-machine-light dispatch:
//!
//! | Event | Synthesises | Side effect |
//! |---|---|---|
//! | `JobStarted` | `SwarmJobEvent::Started` | INSERT swarm_jobs row (source='brain') |
//! | `TaskDispatch` | `SwarmJobEvent::StageStarted` | none |
//! | `AgentResult` | `SwarmJobEvent::StageCompleted` | INSERT swarm_stages row |
//! | `AgentHelpRequest` | (no SwarmJobEvent — surfaced in agent pane only) | none |
//! | `CoordinatorHelpOutcome` | (no SwarmJobEvent) | none |
//! | `JobCancel` | `SwarmJobEvent::Cancelled` | UPDATE swarm_jobs.state='failed', last_error='cancelled by user' |
//! | `JobFinished` | `SwarmJobEvent::Finished` | UPDATE swarm_jobs.state, finished_at_ms |
//!
//! Retry detection: a `TaskDispatch` whose `target` matches the
//! most recent `AgentResult.target` AND whose chain of
//! `parent_id`s traces back to a Verdict-rejected result counts
//! as a retry. The projector tracks per-job retry counters and
//! emits `SwarmJobEvent::RetryAttempt` before the StageStarted
//! for the retry dispatch.

use std::collections::HashMap;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::swarm::coordinator::{
    Job, JobOutcome, JobState, StageResult, SwarmJobEvent,
};
use crate::swarm::mailbox_bus::{MailboxBus, MailboxEnvelope, MailboxEvent};

pub struct JobProjector;

impl JobProjector {
    /// Spawn the projector task for one workspace. Subscribes to
    /// the bus; loops on `recv()`. Lives until workspace shutdown.
    pub fn spawn<R: Runtime>(
        app: AppHandle<R>,
        workspace_id: String,
        bus: Arc<MailboxBus>,
        pool: DbPool,
    ) -> ProjectorHandle;

    /// Walk the entire mailbox event log for a job and compute the
    /// final `JobOutcome`. Used by `swarm:run_job_v2` to return a
    /// JobOutcome at IPC return time, and by `swarm:get_job` to
    /// hydrate brain-driven jobs.
    pub async fn build_outcome(
        bus: &Arc<MailboxBus>,
        pool: &DbPool,
        job_id: &str,
    ) -> Result<JobOutcome, AppError>;
}

pub struct ProjectorHandle {
    handle: tokio::task::JoinHandle<()>,
    shutdown: Arc<tokio::sync::Notify>,
}

impl ProjectorHandle {
    pub async fn shutdown(self);
}
```

### 3. Wire-up `swarm:run_job_v2`

`commands/swarm.rs::swarm_run_job_v2` (W5-03) is updated:
- After spawning `CoordinatorBrain::run`, await its `JobFinished`.
- Once finished, call `JobProjector::build_outcome(bus, pool,
  &job_id)` to derive the final `JobOutcome`.
- Return the JobOutcome.

The projector task is spawned by `lib.rs::run` setup hook (one
per workspace, lazy on first `JobStarted` per workspace), NOT
per-IPC-call. The brain task and projector task run side-by-side.

### 4. `Job` struct extension

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    // ... existing fields ...
    /// W5-04: 'fsm' for W3 FSM-driven jobs, 'brain' for W5-03
    /// brain-driven jobs. Defaults to 'fsm' on deserialise so
    /// older persisted rows round-trip unchanged.
    #[serde(default = "Job::default_source")]
    pub source: String,
}

impl Job {
    fn default_source() -> String { "fsm".into() }
}
```

`store::row_to_job` reads the new column; `store::insert_job`
writes it. The projector inserts brain-driven rows with
`source='brain'`; the FSM inserts with `source='fsm'`.

### 5. Stage row synthesis

`swarm_stages` rows are inserted by the projector when an
`AgentResult` arrives. The `state` field is mapped from the
agent_id:

| `agent_id` | `state` |
|---|---|
| `scout` | `Scout` |
| `coordinator` (when invoked as Classify) | `Classify` |
| `planner` | `Plan` |
| `backend-builder` / `frontend-builder` | `Build` |
| `backend-reviewer` / `frontend-reviewer` | `Review` |
| `integration-tester` | `Test` |

`specialist_id` = the agent_id verbatim. `assistant_text`,
`session_id`, `total_cost_usd`, `duration_ms` come from the
AgentResult event. `verdict_json` is populated for Reviewer/Tester
by parsing the `assistant_text` (reusing W3-12d's `parse_verdict`).
`decision_json` is populated for Classify-ish dispatches (rare in
brain-driven flow; left null usually).

### 6. Retry attempt detection

Pseudocode:

```rust
fn is_retry_dispatch(
    dispatch: &MailboxEnvelope,
    history: &[MailboxEnvelope],
) -> Option<u32> {
    let MailboxEvent::TaskDispatch { target, .. } = &dispatch.event else { return None; };
    // Walk history for a previous AgentResult with the same target
    // whose Verdict was rejected (parse the assistant_text).
    let mut prior_dispatches_to_target = 0;
    for h in history.iter().rev() {
        if let MailboxEvent::TaskDispatch { target: t, .. } = &h.event {
            if t == target {
                prior_dispatches_to_target += 1;
            }
        }
    }
    if prior_dispatches_to_target > 0 {
        Some(prior_dispatches_to_target as u32 + 1) // 1-indexed retry counter
    } else {
        None
    }
}
```

The projector emits `SwarmJobEvent::RetryAttempt` before the
matching `StageStarted` so frontend listeners see the same
ordering they get from the FSM today.

### 7. `swarm:list_jobs` / `swarm:get_job` updates

Both IPCs already query `swarm_jobs` / `swarm_stages`. After
W5-04, those tables include brain-driven rows. No IPC changes
required — the SQL covers both sources by default.

Frontend hooks (`useSwarmJob`, `useSwarmJobs`) expose `source` if
present; the existing UI ignores it (no rendering branch on
source). A future polish could add a small "v1/v2" pill on each
row.

### 8. Bindings regen

`pnpm gen:bindings` produces:
- `Job.source` field (added)
- No new commands (none in W5-04)

## Out of scope

- ❌ FSM teardown (W5-06)
- ❌ Cancel migration (W5-05)
- ❌ Frontend "v1/v2 pill" UI affordance (post-W5)
- ❌ Workspace_id column on mailbox (still single-workspace)
- ❌ Projector recovery on app restart — for W5 the projector
  starts fresh on each launch; brain-driven jobs that were
  in-flight at shutdown are recovered as `Failed { last_error:
  'interrupted by app restart' }` via the same `recover_orphans`
  sweep as FSM-driven jobs (the swarm_jobs row has a non-terminal
  state at recovery time).

## Acceptance criteria

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` ≥ baseline + 15
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regen + commit
- [ ] `pnpm gen:bindings:check` exits 0
- [ ] `pnpm typecheck` / `pnpm lint` / `pnpm test --run` exit 0
- [ ] Tests:
  - [ ] migration_0011_round_trip
  - [ ] projector_emits_started_on_job_started
  - [ ] projector_emits_stage_started_on_task_dispatch
  - [ ] projector_emits_stage_completed_on_agent_result
  - [ ] projector_inserts_swarm_jobs_row_with_brain_source
  - [ ] projector_inserts_swarm_stages_row
  - [ ] projector_maps_agent_id_to_correct_job_state
  - [ ] projector_emits_retry_attempt_on_repeated_dispatch
  - [ ] projector_does_not_emit_retry_on_first_dispatch
  - [ ] projector_emits_finished_on_job_finished_done
  - [ ] projector_emits_finished_on_job_finished_failed
  - [ ] projector_emits_cancelled_then_finished_on_job_cancel
  - [ ] build_outcome_walks_event_log_correctly
  - [ ] build_outcome_handles_brain_driven_job_with_retry
  - [ ] swarm_get_job_returns_brain_driven_job_in_legacy_shape

## Verification commands

Standard W5 cargo + pnpm gates.

## Files allowed to modify

- `src-tauri/migrations/0011_swarm_jobs_source.sql` (new)
- `src-tauri/src/swarm/projector.rs` (new)
- `src-tauri/src/swarm/mod.rs` (re-exports)
- `src-tauri/src/swarm/coordinator/job.rs` (add `source` field)
- `src-tauri/src/swarm/coordinator/store.rs` (read/write `source`)
- `src-tauri/src/db.rs` (migration count 10 → 11)
- `src-tauri/src/commands/swarm.rs` (`swarm_run_job_v2` calls
  `JobProjector::build_outcome`)
- `src-tauri/src/lib.rs` (manage projector spawning)
- `app/src/lib/bindings.ts` (regen)
- `docs/work-packages/WP-W5-04-job-state-projector.md`
- `AGENT_LOG.md`

MUST NOT touch:
- FSM (`swarm/coordinator/fsm.rs`)
- Mailbox bus surface (W5-01)
- Brain (W5-03)
- Persona files
- Frontend components (no UI changes in W5)

## Notes / risks

- **State machine drift**: the projector's mapping of
  agent_id → JobState is hardcoded. If a future persona is added
  (post-W5), the mapping needs updating. Document inline.
- **Verdict parse failures**: `assistant_text` for Reviewer/Tester
  may not parse as Verdict (LLM output drift). Projector logs
  `tracing::warn!` and writes `verdict_json=NULL` rather than
  failing the projection — same fail-soft as the FSM today.
- **Race between brain emit and projector consume**: the
  projector subscribes BEFORE the first emit (spawned by setup
  hook on app launch). The bus's broadcast channel is FIFO per
  subscriber so order is preserved. If the projector is restarted
  mid-job (it isn't in W5, but the future "Swarm comms" tab might
  spin one up), it can replay missed events via
  `mailbox:list_typed(since_id)`.
- **JobOutcome.last_verdict**: derived from the most recent
  `AgentResult` with `verdict.approved == false`. If the brain
  emits `JobFinished{outcome:"failed"}` after a verdict
  rejection, the projector ties the failure to the verdict. If
  the brain finishes failed for another reason (max dispatches,
  cancel, error), `last_verdict` is None and `last_error` carries
  the brain's `JobFinished.summary`.

---
id: WP-W3-12b
title: Coordinator FSM — SQLite persistence + restart recovery
owner: TBD
status: not-started
depends-on: [WP-W3-12a]
acceptance-gate: "JobRegistry writes through to SQLite at every state transition. App restart while a job is in flight finalizes the orphan as `Failed { last_error: 'interrupted by app restart' }` and clears the workspace lock. `swarm:list_jobs(workspace_id?)` and `swarm:get_job(job_id)` IPC return the persisted history. Existing W3-12a/c integration tests still pass against the SQLite-backed registry."
---

## Goal

Stop losing in-flight Coordinator job state on app restart, and
expose a job history surface to the IPC for W3-14's UI.

This WP does NOT change the FSM mechanics, the streaming
protocol, or any user-visible UX beyond "jobs persist." It is
the boring durability layer underneath W3-12a + W3-12c.

## Why now / scope justification

W3-12a's `JobRegistry` is an `Arc<Mutex<HashMap>>` — every job
evaporates when the user closes the window. W3-12c's events
help while a job runs but don't help "where did my last hour
of swarm work go?"

Three concrete pains this WP closes:

1. **Restart loses in-flight state.** The user sees no record
   of a 2-minute job that was running when they hit Cmd-Q.
2. **No history view possible.** W3-14 wants a "your last 10
   jobs" panel. Today there's nowhere to read from.
3. **Workspace lock leak.** A panic mid-FSM that bypasses
   `WorkspaceGuard` (impossible today, but the lock-leak
   surface area grows with W3-12d's retry/Verdict logic) leaves
   `workspace_locks` unrecoverable until the next process exit.
   SQLite-backed locks survive crashes the same way the rest of
   the schema does.

W3-14 (UI) cannot land without this — the multi-pane UI's
"recent jobs" surface needs a real query.

## Charter alignment

No tech-stack change. SQLite is already the persistence layer
(WP-W2-02); we're just adding three tables. Migration cadence
follows the existing pattern (`NNNN_name.sql`).

## Scope

### 1. Migration `0006_swarm_jobs.sql`

```sql
CREATE TABLE swarm_jobs (
  id              TEXT    PRIMARY KEY,
  workspace_id    TEXT    NOT NULL,
  goal            TEXT    NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  state           TEXT    NOT NULL,    -- "init"|"scout"|"plan"|"build"|"review"|"test"|"done"|"failed"
  retry_count     INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,                -- nullable; set when state=failed
  finished_at_ms  INTEGER              -- nullable; set when state in (done, failed)
) WITHOUT ROWID;

CREATE INDEX idx_swarm_jobs_workspace ON swarm_jobs (workspace_id);
CREATE INDEX idx_swarm_jobs_state ON swarm_jobs (state);
CREATE INDEX idx_swarm_jobs_created ON swarm_jobs (created_at_ms);

CREATE TABLE swarm_stages (
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  idx             INTEGER NOT NULL,
  state           TEXT    NOT NULL,
  specialist_id   TEXT    NOT NULL,
  assistant_text  TEXT    NOT NULL,
  session_id      TEXT    NOT NULL,
  total_cost_usd  REAL    NOT NULL,
  duration_ms     INTEGER NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  PRIMARY KEY (job_id, idx)
) WITHOUT ROWID;

CREATE TABLE swarm_workspace_locks (
  workspace_id    TEXT    PRIMARY KEY,
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  acquired_at_ms  INTEGER NOT NULL
) WITHOUT ROWID;
```

`WITHOUT ROWID` for jobs (UUID-string PK) and locks
(workspace_id PK) saves the implicit rowid btree level.
`swarm_stages` keeps rowid because its composite PK
`(job_id, idx)` is naturally an int-trailing key — without-
rowid would be a marginal pessimization there.

### 2. `JobRegistry` write-through to SQLite

`JobRegistry` keeps its existing in-memory shape — three
mutex-guarded maps for jobs, workspace_locks, cancel_notifies —
but gains an optional `pool: Option<DbPool>`. Every mutation
that touches `jobs` or `workspace_locks` writes through.

```rust
pub struct JobRegistry {
    jobs: Mutex<HashMap<String, Job>>,
    workspace_locks: Mutex<HashMap<String, String>>,
    cancel_notifies: Mutex<HashMap<String, Arc<Notify>>>,
    pool: Option<DbPool>,    // None for tests; Some for production
}

impl JobRegistry {
    pub fn new() -> Self;                    // existing; in-memory only
    pub fn with_pool(pool: DbPool) -> Self;  // production
}
```

Methods that change behavior:

- `try_acquire_workspace`: in-memory insert AS BEFORE, then on success, fire-and-forget the SQL inserts (job row + workspace_lock row) under one tx. SQL failure → roll back the in-memory insert and surface `AppError::Internal("...")`.
- `update`: in-memory mutation AS BEFORE, then re-serialize the resulting `Job` and UPDATE its row. New stages (delta vs. previous `stages.len()`) get INSERTed.
- `release_workspace`: in-memory remove AS BEFORE, then DELETE the workspace_lock row. ON DELETE CASCADE on `swarm_jobs(id)` doesn't fire because the job stays — only the lock row is removed.

`cancel_notifies` writes are NOT persisted — cancel state is process-local. After restart, no cancel pending.

The async-vs-sync question: `JobRegistry`'s methods are currently
synchronous (taking `&self`). To call `sqlx` we need async. Two
choices:

- **Make every method async.** Cleanest. Callers already await on the FSM.
- **Spawn a tokio task for each write.** Keeps the surface sync, but introduces fire-and-forget semantics (a write failure during a test would race the assertion).

Pick **async**. All mutators (`try_acquire_workspace`, `update`,
`release_workspace`) become `async fn`. Read-only methods
(`get`, `list`) stay sync because they just read in-memory.
`signal_cancel` stays sync (no SQL).

### 3. Restart recovery

On app startup, before exposing `JobRegistry` via `app.manage`,
the registry runs a recovery pass:

```rust
impl JobRegistry {
    /// Sweep orphan jobs left non-terminal at process start.
    /// Called once from `lib.rs::setup` BEFORE `app.manage`.
    pub async fn recover_orphans(&self) -> Result<usize, AppError> {
        // 1. UPDATE swarm_jobs SET state='failed',
        //    last_error='interrupted by app restart',
        //    finished_at_ms=:now
        //    WHERE state NOT IN ('done','failed');
        // 2. DELETE FROM swarm_workspace_locks; (cascade-safe; no
        //    FK constraint anymore since the job rows still exist
        //    but flipped to failed)
        // 3. SELECT * FROM swarm_jobs ORDER BY created_at_ms DESC
        //    LIMIT 100, hydrate `jobs` map (capped to recent 100
        //    so a long-lived install doesn't OOM).
        // Returns count of orphans recovered for logging.
    }
}
```

The 100-row cap is an MVP pragmatism — full pagination is W3-14
(UI side). The `swarm:list_jobs` IPC still hits the DB, not the
in-memory cache.

`lib.rs::setup` flow:

```rust
let registry = Arc::new(JobRegistry::with_pool(pool.clone()));
let recovered = tauri::async_runtime::block_on(registry.recover_orphans())?;
if recovered > 0 {
    tracing::warn!(count = recovered, "swarm: recovered N orphan jobs");
}
app.manage(registry);
```

### 4. New Tauri commands

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_list_jobs<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<JobSummary>, AppError>;

#[tauri::command]
#[specta::specta]
pub async fn swarm_get_job<R: Runtime>(
    app: AppHandle<R>,
    job_id: String,
) -> Result<JobDetail, AppError>;
```

`JobSummary` is a slim wire-shape (no full assistant_text per
stage) used for the recent-jobs panel:

```rust
pub struct JobSummary {
    pub id: String,
    pub workspace_id: String,
    pub goal: String,                // truncated to 200 chars on the wire
    pub created_at_ms: i64,
    pub finished_at_ms: Option<i64>,
    pub state: JobState,
    pub stage_count: u32,            // not the full Vec<StageResult>
    pub total_cost_usd: f64,
    pub last_error: Option<String>,
}
```

`JobDetail` is the full thing — same fields as `Job` from
W3-12a plus `stages: Vec<StageResult>`. Used for the inspector
view.

`limit` defaults to 50 if unset; capped at 200 to prevent
runaway queries. Workspace filter, when set, hits the index.

### 5. SQL helpers

A new sibling module `src-tauri/src/swarm/coordinator/store.rs`
holds the SQL:

```rust
pub(super) async fn insert_job_and_lock(
    pool: &DbPool,
    job: &Job,
    workspace_id: &str,
    acquired_at_ms: i64,
) -> Result<(), AppError>;

pub(super) async fn update_job(
    pool: &DbPool,
    job: &Job,
) -> Result<(), AppError>;

pub(super) async fn insert_stage(
    pool: &DbPool,
    job_id: &str,
    idx: u32,
    stage: &StageResult,
) -> Result<(), AppError>;

pub(super) async fn delete_workspace_lock(
    pool: &DbPool,
    workspace_id: &str,
) -> Result<(), AppError>;

pub(super) async fn list_jobs(
    pool: &DbPool,
    workspace_id: Option<&str>,
    limit: u32,
) -> Result<Vec<JobSummary>, AppError>;

pub(super) async fn get_job_detail(
    pool: &DbPool,
    job_id: &str,
) -> Result<Option<JobDetail>, AppError>;

pub(super) async fn recover_orphans(
    pool: &DbPool,
    now_ms: i64,
) -> Result<RecoveredOrphans, AppError>;
```

`pub(super)` — these are FSM-internal. The Tauri commands call
through `JobRegistry` only.

### 6. JobState ↔ string mapping

The DB stores state as a TEXT column. Map:

```rust
impl JobState {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            JobState::Init => "init",
            JobState::Scout => "scout",
            JobState::Plan => "plan",
            JobState::Build => "build",
            JobState::Review => "review",
            JobState::Test => "test",
            JobState::Done => "done",
            JobState::Failed => "failed",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self, AppError> {
        match s {
            "init" => Ok(JobState::Init),
            "scout" => Ok(JobState::Scout),
            "plan" => Ok(JobState::Plan),
            "build" => Ok(JobState::Build),
            "review" => Ok(JobState::Review),
            "test" => Ok(JobState::Test),
            "done" => Ok(JobState::Done),
            "failed" => Ok(JobState::Failed),
            other => Err(AppError::Internal(format!(
                "unknown swarm job state in DB: {other}"
            ))),
        }
    }
}
```

The string repr matches `serde(rename_all = "snake_case")` on
the JS-facing wire enum, so DB values and frontend values agree
(which simplifies the W3-14 hook).

### 7. Tests

#### Unit tests (in-memory SQLite via `mock_app_with_pool`)

- `migration_0006_creates_three_tables` — count goes from 5 → 6;
  the three new tables are present.
- `insert_job_and_lock_round_trip` — write a Job via
  `try_acquire_workspace`, `SELECT * FROM swarm_jobs WHERE id=?`
  shows it.
- `update_job_persists_state_and_stage_count` — drive a Job
  through SCOUT/PLAN/BUILD/DONE via `update`; verify each
  intermediate state lands in the DB.
- `insert_stage_appends_to_job` — push a StageResult; the
  `swarm_stages` row is written with the right `idx`.
- `release_workspace_deletes_lock_row` — call
  `release_workspace`; row is gone.
- `recover_orphans_flips_non_terminal_jobs_to_failed` — seed
  the DB with a job in state=scout, run `recover_orphans`;
  state=failed, `last_error` set, `finished_at_ms` populated.
- `recover_orphans_releases_workspace_locks` — seed lock + job
  combo; orphan recovery clears the lock row.
- `recover_orphans_leaves_terminal_jobs_alone` — seed a Done
  and a Failed; recovery does not touch them.
- `list_jobs_filters_by_workspace` — seed 2 workspaces × 3 jobs;
  filter returns the right 3.
- `list_jobs_truncates_goal_to_200_chars` — seed a job with a
  500-char goal; `JobSummary.goal.len() <= 200`.
- `list_jobs_respects_limit` — seed 100 jobs; `limit=10` returns 10.
- `list_jobs_orders_by_created_desc` — seed 3 jobs at distinct
  timestamps; results ordered newest-first.
- `get_job_detail_returns_full_stages` — seed a job with 3
  stages; detail returns all 3 in order.
- `get_job_detail_unknown_returns_none` — unknown id → Ok(None).
- `swarm_list_jobs_command_returns_summaries` — IPC test via
  the existing mock app builder.
- `swarm_get_job_command_returns_detail`.
- `swarm_get_job_unknown_returns_not_found_error` — Ok(None)
  from store maps to `AppError::NotFound` at the IPC layer.

#### FSM regression (in-memory pool)

Every existing FSM unit test (W3-12a + W3-12c) re-run with a
`JobRegistry::with_pool(in_memory_pool)` instead of
`JobRegistry::new()`. Confirms the in-memory and SQLite-backed
paths are behavior-compatible.

Add a parameterized helper:

```rust
async fn registry_for_test(pool: bool) -> Arc<JobRegistry> {
    if pool {
        let pool = test_pool().await;
        Arc::new(JobRegistry::with_pool(pool))
    } else {
        Arc::new(JobRegistry::new())
    }
}
```

Each existing test gets a sibling with `pool=true` so the FSM
is exercised against both backends.

#### Integration test (`#[ignore]`)

- `integration_persistence_survives_real_claude_chain` — run the
  W3-12a happy path against a SQLite-backed registry; after
  completion, assert the DB has one Done job + 3 stage rows + 0
  workspace_locks. Reuses the canonical `profile_count` goal.

Test count target: ≥17 unit + 1 ignored integration. New
baseline ≥240 passing.

### 8. Bindings regen

`pnpm gen:bindings` adds:
- `commands.swarmListJobs(workspaceId?, limit?) -> Promise<JobSummary[]>`
- `commands.swarmGetJob(jobId) -> Promise<JobDetail>`
- New types: `JobSummary`, `JobDetail`

`pnpm gen:bindings:check` exits 0 post-commit.

## Out of scope

- ❌ Resume an interrupted job (continue from last completed
  stage). 12b only marks orphans as Failed. Resume is W3-12d
  via the retry surface.
- ❌ Pagination beyond the 200-row hard cap. W3-14 may add
  cursor-based pagination if the recent-jobs panel grows.
- ❌ Trim policy (delete jobs older than N days). Separate
  W3-12b+ sweep, parallels the W3-06 OTel trim.
- ❌ Multi-window job filtering. `app.emit` already broadcasts
  to all windows; no per-window filter.
- ❌ Backfill of W3-12a/12c jobs that ran before this WP — the
  schema is created fresh; no historical data exists.
- ❌ Cross-process coordination via SQLite advisory locks. We
  assume one Neuron process per install (matches Charter
  Phases row).

## Acceptance criteria

- [ ] `migrations/0006_swarm_jobs.sql` exists, three tables
      created, three indexes on `swarm_jobs`
- [ ] Migration count test bumps from 5 → 6 (existing
      `migrations_are_idempotent` etc. updated)
- [ ] `JobRegistry::with_pool(pool)` constructor; existing
      `new()` keeps working (in-memory) for tests
- [ ] All `JobRegistry` mutators that touch `jobs` or
      `workspace_locks` write through to SQLite
- [ ] `recover_orphans` flips non-terminal → failed, clears
      locks, hydrates the in-memory cache (capped 100 rows)
- [ ] `lib.rs::setup` calls `recover_orphans` before
      `app.manage(registry)`
- [ ] `swarm:list_jobs(workspace_id?, limit?)` IPC compiles,
      types end-to-end; `JobSummary` shape per §4
- [ ] `swarm:get_job(job_id)` IPC compiles; `JobDetail` shape
      per §4; unknown id → `AppError::NotFound`
- [ ] `JobState::{as,from}_db_str` round-trip on every variant
- [ ] No new dep, no `unsafe`, no `eprintln!`
- [ ] All Week-2 + Week-3-prior tests still pass; target ≥240
      passing
- [ ] Integration test (`#[ignore]`d) compiles; orchestrator
      runs it post-commit
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check`
      exits 0 post-commit

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
cargo test --lib -- integration_persistence_survives_real_claude_chain --ignored --nocapture
cargo test --lib -- integration_fsm_drives_real_claude_chain --ignored --nocapture
cargo test --lib -- integration_cancel_during_real_claude_chain --ignored --nocapture
```

## Notes / risks

- **Async cascade.** Making `JobRegistry` mutators async
  ripples into `CoordinatorFsm::run_job` (already async, no
  problem) and into the new `lib.rs::setup` block-on call.
  Read-only methods stay sync to avoid forcing IPC commands
  to await for trivial lookups.
- **Write-through latency.** Each `update` does one SQL UPDATE
  + maybe one stage INSERT. SQLite write latency is ~1-3ms on
  WAL mode. Per FSM stage we pay this once on
  `StageStarted`-driven update and once on
  `StageCompleted`-driven update — negligible vs. the 10-30s
  claude spawn.
- **Schema migration impact on existing tests.** Several DB
  tests count tables (`migration_creates_all_expected_tables`
  in W3-W2-02 era). They need to grow to expect 6 migrations
  and 14 tables (existing 11 + 3 new).
- **Orphan recovery is destructive of in-flight context.** A
  user who legitimately closed the app mid-job and reopened
  expecting it to continue gets a `Failed` instead. W3-12d's
  retry can re-run the orphaned goal; W3-14 surfaces a "rerun"
  button. Document the behavior in the commit message so the
  expectation is clear.
- **`Job` finally lands in bindings.** W3-12a deliberately
  excluded it because no IPC returned it. Now `swarm:get_job`
  returns `JobDetail` (which carries the same shape sans the
  in-memory bookkeeping fields), so `JobDetail` is the public
  type and `Job` stays internal.
- **Cancel survives orphan recovery as Failed.** A job that
  was `cancelled by user` mid-flight at process kill becomes
  `interrupted by app restart` post-recovery. The user-facing
  difference doesn't matter (both Failed); the audit trail
  loses the cancel reason. Acceptable.
- **Lock acquisition order extended again.** With write-through
  SQL, the order is now `workspace_locks (in-mem) → jobs (in-mem) → DB tx`.
  All in-memory locks released BEFORE awaiting on the DB.
  Document in `JobRegistry`'s `// LOCK ORDER:` comment.

## Sub-agent reminders

- Read this WP in full before writing code.
- Read `src-tauri/migrations/0005_span_export.sql` for the
  WP-W3-06 migration style; mirror it.
- Read `src-tauri/src/db.rs` for the existing migration test
  patterns (`migration_creates_all_eleven_tables` etc.); update
  them rather than rewriting.
- Read `src-tauri/src/swarm/coordinator/{job,fsm}.rs` (W3-12a
  + W3-12c) for the existing registry surface; `with_pool` and
  the async mutators are the only structural change.
- DO NOT add a new dep. `sqlx` is already in tree with the
  features we need.
- DO NOT change the FSM's stage prompt templates or the
  `swarm:run_job` / `swarm:cancel_job` / `swarm:test_invoke`
  IPC signatures.
- DO NOT introduce `tokio::task::spawn` for SQL writes —
  fire-and-forget semantics are forbidden because tests would
  race the assertion. Mutators are async and await SQL inline.
- DO NOT add cross-table SQL transactions beyond the one used
  by `try_acquire_workspace` (job + lock atomic insert). Each
  stage insert and each job update can be its own statement.
- DO NOT touch `runs` / `runs_spans` schema — that's the
  LangGraph runtime, separate cleanup story.
- Per `AGENTS.md`: one WP = one commit.

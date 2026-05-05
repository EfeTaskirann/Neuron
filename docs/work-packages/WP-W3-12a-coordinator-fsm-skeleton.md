---
id: WP-W3-12a
title: Coordinator FSM skeleton — in-memory, blocking, 3-state happy path
owner: TBD
status: not-started
depends-on: [WP-W3-11]
acceptance-gate: "`swarm:run_job(workspace_id: String, goal: String) -> Result<JobOutcome, AppError>` walks SCOUT → PLAN → BUILD → DONE through the substrate from WP-W3-11, dispatching the bundled `scout` / `planner` / `backend-builder` profiles in turn. Caller blocks on a single IPC; final `JobOutcome` carries every specialist's output. Same `workspace_id` serializes (second call rejected with `WorkspaceBusy`); different `workspace_id`s run in parallel. No DB, no streaming."
---

## Goal

Land the FSM that turns Phase 1's per-invoke substrate
(WP-W3-11) into a chained mini-workflow. One Tauri command —
`swarm:run_job` — accepts a free-text goal, walks the three
bundled profiles in a fixed order, and returns the final
`JobOutcome` once the BUILD stage produces an artifact (or the
chain bails earlier).

This WP is the FSM **skeleton**:

- Pure Rust state machine; no Coordinator LLM brain (deferred to
  W3-12d, see "Architectural rationale").
- In-memory job state; restart wipes everything (deferred to
  W3-12b — DB persistence).
- Blocking IPC; caller awaits the single Promise (streaming
  deltas to the frontend deferred to W3-12c).
- 3-state happy path: SCOUT → PLAN → BUILD → DONE. REVIEW / TEST
  states are defined in the enum but unreachable until W3-12d
  authors `reviewer.md` + `integration-tester.md` profiles.

The acceptance gate is the manual mini-flow the owner already
validated with three direct `claude -p` calls on 2026-05-05 — the
FSM packages that chain into a single IPC.

## Why now / scope justification

WP-W3-11 proved the substrate works for one specialist at a
time. The owner's manual chain validation (3 successive
`claude -p --append-system-prompt-file` calls against the bundled
scout / planner / backend-builder profiles, scout findings →
planner plan → builder dry-run) confirmed the personas hand off
cleanly. W3-12a is the codified version of that chain: same
inputs, same outputs, but driven by Rust state machine logic and
exposed as a single IPC — the seed for the W3-12 series.

Splitting W3-12 into three sub-WPs (12a/12b/12c) keeps each
landing under M-size and lets the FSM mechanics, persistence
choices, and streaming protocol each get their own scrutiny:

| Sub-WP | Scope | Size | Depends on |
|---|---|---|---|
| **W3-12a (this WP)** | FSM skeleton, in-memory, blocking | M | W3-11 |
| W3-12b | SQLite persistence + restart recovery | M | W3-12a |
| W3-12c | Streaming Tauri events + frontend hook | M | W3-12a, optionally 12b |
| W3-12d | REVIEW/TEST states + reviewer/integration-tester profiles + Verdict schema + retry feedback loop + Coordinator LLM brain (Option B) | L | W3-12a, ideally also 12b |

W3-12d intentionally bundles three concerns (verdict gate +
retry feedback + Coordinator LLM) because they are tightly
coupled: a Verdict schema needs a parser, the retry loop needs
verdicts to feed back, and the Coordinator LLM brain decides
whether to invoke the reviewer at all.

## Charter alignment

No new tech-stack row required. The FSM is pure Rust on top of
the existing "Swarm runtime" row added in WP-W3-11 (Charter
2026-05-05 amendment).

## Architectural rationale (Coordinator brain trade-off)

The architectural report's §5.1 strongly recommends deterministic
flow control with LLM only invoked *inside* a node. Three concrete
implementations of this are possible:

| Option | Description | Phase |
|---|---|---|
| **A** | Pure Rust FSM, hardcoded SCOUT → PLAN → BUILD pipeline; no Coordinator LLM at all | **W3-12a** |
| B | Rust FSM + on-demand single-shot `coordinator.md` brain for routing decisions | W3-12d |
| C | Persistent Coordinator subprocess holding chat context | W3-13+ |

W3-12a takes Option A. Rationale:

- **Smallest validation surface.** FSM mechanics (state transitions,
  retry counter wiring, error handling, output passing) are
  validated in isolation without conflating LLM routing concerns.
- **Reuses bundled profiles.** No new `coordinator.md` to author;
  the substrate from W3-11 (3 profiles) is already exercised.
- **Trivial upgrade path.** The Rust FSM is a state-transition
  table — swapping a hardcoded "next state" for a `coordinator_llm.choose_next(state)` call (Option B in W3-12d) is a 1-2 file refactor. Option A's tests still pass; Option B
  layers on top.
- **Cost predictability.** Job cost = sum of three specialist
  invokes (~$0.03–$0.10). Adding the Coordinator brain (Option B)
  adds one more invoke per job, which is fine but worth measuring
  separately.

The owner's manual mini-flow already proved the hardcoded
pipeline produces useful chain output for at least one
realistic task ("add a `profile_count` helper to ProfileRegistry").
W3-12a packages that exact pipeline into Rust.

## Scope

### 1. New module `src-tauri/src/swarm/coordinator/`

Sibling of `swarm/{binding,profile,transport}.rs` from W3-11:

```
src-tauri/src/swarm/coordinator/
├── mod.rs       // pub mod fsm; pub mod job; pub re-exports
├── fsm.rs       // CoordinatorFsm — state, transitions, run loop
└── job.rs       // Job, JobState, JobOutcome, JobRegistry
```

Wired through `swarm::mod.rs` with `pub mod coordinator;`.

### 2. State and transition types (`coordinator::job`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
pub enum JobState {
    Init,
    Scout,
    Plan,
    Build,
    /// Reserved for W3-12d. FSM never enters this state in W3-12a;
    /// transition table treats it as no-op pass-through.
    Review,
    /// Reserved for W3-12d. Same as Review.
    Test,
    Done,
    /// Terminal failure state. Carries the last error.
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct StageResult {
    pub state: JobState,
    pub specialist_id: String,    // e.g. "scout"
    pub assistant_text: String,
    pub session_id: String,
    pub total_cost_usd: f64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct Job {
    pub id: String,                       // ULID, prefixed `j-`
    pub goal: String,                     // user's input
    pub created_at_ms: i64,
    pub state: JobState,
    pub retry_count: u32,                 // wired but no consumer in 12a
    pub stages: Vec<StageResult>,         // append-only; one per completed stage
    pub last_error: Option<String>,       // populated on Failed
}

#[derive(Debug, Clone, Serialize, Deserialize, specta::Type)]
pub struct JobOutcome {
    pub job_id: String,
    pub final_state: JobState,            // Done or Failed
    pub stages: Vec<StageResult>,
    pub last_error: Option<String>,
    pub total_cost_usd: f64,              // sum across stages
    pub total_duration_ms: u64,
}
```

`JobRegistry` is an in-memory `Arc<Mutex<HashMap<String, Job>>>`
managed via `tauri::Manager::manage`. W3-12b replaces it with a
SQLite-backed equivalent; the public method surface stays the
same so callers don't churn.

```rust
pub struct JobRegistry { /* ... */ }
impl JobRegistry {
    pub fn new() -> Self;
    pub fn insert(&self, job: Job);
    pub fn update<F: FnOnce(&mut Job)>(&self, id: &str, f: F) -> Result<(), AppError>;
    pub fn get(&self, id: &str) -> Option<Job>;
    pub fn list(&self) -> Vec<Job>;       // for the W3-12c streaming list
}
```

`MAX_RETRIES = 2` is a `pub const` on `coordinator::fsm` per the
architectural report §5.3. The constant is used by `Failed` state
guards once the Verdict gate lands in W3-12d. In W3-12a no
retry logic fires; the constant is exported so 12d doesn't have
to relitigate it.

### 3. State machine (`coordinator::fsm`)

```rust
pub struct CoordinatorFsm {
    profiles: Arc<ProfileRegistry>,
    transport: SubprocessTransport,
    registry: Arc<JobRegistry>,
    /// Per-stage timeout budget. Default 60s (matches W3-11
    /// `SubprocessTransport::invoke` default). Configurable via
    /// `NEURON_SWARM_STAGE_TIMEOUT_SEC` at FSM construction time.
    stage_timeout: Duration,
}

impl CoordinatorFsm {
    pub fn new(
        profiles: Arc<ProfileRegistry>,
        registry: Arc<JobRegistry>,
        stage_timeout: Duration,
    ) -> Self;

    /// Drive a job from INIT to DONE/FAILED. Blocking; returns
    /// the final outcome. Mutates `registry` at every transition.
    pub async fn run_job(&self, app: &AppHandle<R>, goal: String) -> Result<JobOutcome, AppError>;
}
```

**Transition table (W3-12a)**:

```
INIT          → SCOUT  (always)
SCOUT(ok)     → PLAN
SCOUT(err)    → FAILED
PLAN(ok)      → BUILD
PLAN(err)     → FAILED
BUILD(ok)     → DONE
BUILD(err)    → FAILED
REVIEW        → unreachable in 12a; transition table contains a `debug_assert!(false)` guard
TEST          → unreachable in 12a; same guard
DONE / FAILED → terminal
```

**Stage prompt construction**:

- **SCOUT**: prompt = the user's `goal` verbatim. (Scout's persona
  already says "use Read/Grep/Glob to investigate"; goal-only
  input matches the manual validation pattern.)
- **PLAN**: prompt = a fixed template:

  ```
  Hedef: {goal}

  Scout bulguları:

  {scout_assistant_text}

  Bu hedef için adım adım bir plan üret.
  ```

- **BUILD**: prompt = a fixed template:

  ```
  Aşağıdaki Plan'ın 1. adımını uygula.

  {plan_assistant_text}

  ŞU ANDA SADECE ADIM 1'İ UYGULA.
  ```

The "step 1 only" instruction matches the manual validation;
multi-step build is a W3-12d concern (the FSM iterates BUILD
once per remaining plan step, gated by a "next step?" verdict).

Templates are `const &str` in `fsm.rs` with a `format!` wrapper —
no template engine dep.

**Error mapping**: any `SubprocessTransport::invoke` failure
inside a stage flips `Job.state = Failed`, populates
`Job.last_error`, and `run_job` returns `Ok(JobOutcome { final_state: Failed, .. })`. The IPC surface returns
`Result<JobOutcome, AppError>`; transport errors are caught
*inside* the FSM so the IPC always returns Ok with a structured
outcome unless the FSM itself can't load profiles or spawn at
all (in which case `AppError`).

### 4. Tauri command `swarm:run_job` + per-workspace lock

New command in `src-tauri/src/commands/swarm.rs` (sibling of the
existing two from W3-11):

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_run_job<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    goal: String,
) -> Result<JobOutcome, AppError>;
```

Resolves: `ProfileRegistry::load_from(Some(<app_data_dir>/agents/))`,
acquires the workspace lock (see below), constructs
`CoordinatorFsm`, calls `run_job(&app, workspace_id.clone(), goal)`,
releases the lock on completion, returns the outcome. Each call
gets a fresh FSM instance — there is no shared FSM in 12a; the
registry IS the shared state.

#### Per-workspace concurrency policy (owner directive 2026-05-05)

> "Aynı proje için yeni bir 9 kişilik ekibi çalıştırmama izin
> vermesin, başka bir proje için izin versin."

Same workspace = serialize (reject the second call); different
workspaces = parallel (independent FSMs).

`JobRegistry` gains a `workspace_locks` map alongside the jobs
map:

```rust
pub struct JobRegistry {
    jobs: Mutex<HashMap<String /* job_id */, Job>>,
    /// Workspace -> job_id of the currently in-flight job in
    /// that workspace. Insert on `swarm:run_job` start; remove
    /// on completion (Done or Failed).
    workspace_locks: Mutex<HashMap<String /* workspace_id */, String /* job_id */>>,
}

impl JobRegistry {
    /// Atomically: check workspace not busy, register the new
    /// job + lock. Returns the new job, or
    /// `AppError::WorkspaceBusy { workspace_id, in_flight_job_id }`
    /// if the workspace already has an in-flight job.
    pub fn try_acquire_workspace(
        &self,
        workspace_id: &str,
        new_job: Job,
    ) -> Result<(), AppError>;

    /// Remove the workspace lock + finalize the job's terminal
    /// state. Idempotent — calling twice is a no-op (defensive
    /// for the panic-unwind path).
    pub fn release_workspace(&self, workspace_id: &str, job_id: &str);
}
```

Both maps are guarded by independent mutexes; a job mutation
that doesn't change workspace state never touches the lock map.
The `try_acquire_workspace` operation grabs both mutexes in
order (workspace_locks → jobs) to keep the acquire atomic from a
caller's perspective.

`AppError::WorkspaceBusy { workspace_id, in_flight_job_id }` is a
new variant. It is returned **as `Err(...)`**, NOT as
`JobOutcome.final_state = Failed` — this is a pre-flight
rejection, not a job that ran and failed.

#### What `workspace_id` means in W3-12a

Neuron currently has no formal multi-workspace UI. Callers pass
a string they own — typical values:

- `"default"` — single-workspace mode; effectively a global lock.
  This is what the manual smoke uses.
- `"<absolute-project-path>"` — once Neuron grows a
  workspace-picker, the frontend hook passes the project root
  here.
- Any UUID — for callers that want isolation without committing
  to a path scheme.

The string is opaque to the FSM; only the lock map keys on it.
12a does not validate the format — empty string is rejected
(`AppError::InvalidInput`), anything else is accepted.

`JobRegistry` is `app.manage`d in `lib.rs::run()::setup` so
multiple concurrent `swarm:run_job` calls share the same lock
state. Two concurrent calls with **different** `workspace_id`s
run independently; two with the **same** `workspace_id` see the
second call rejected immediately.

Registered in `lib.rs::specta_builder_for_export` under the
existing `// swarm` block.

### 5. `lib.rs` setup wiring

Add to `setup`:

```rust
let job_registry = Arc::new(crate::swarm::coordinator::JobRegistry::new());
app.manage(job_registry);
```

ProfileRegistry is NOT pre-loaded at startup — `swarm:run_job`
loads it lazily on each call (matches the W3-11 cadence for
`swarm:profiles_list`). W3-12b may cache.

### 6. Tests (target: +12 to +18 unit tests)

#### `coordinator::job` tests

- `job_state_transitions_serialize_round_trip` — every variant
  serde-roundtrips through specta's wire shape.
- `job_registry_insert_and_get_roundtrip`.
- `job_registry_update_modifies_in_place`.
- `job_registry_list_returns_all`.
- `job_registry_concurrent_inserts_are_safe` — spawn 8 tokio
  tasks each inserting a job; assert all 8 land.
- `try_acquire_workspace_first_caller_wins` — two concurrent
  `try_acquire_workspace` calls with the same workspace_id;
  exactly one returns Ok, the other returns
  `AppError::WorkspaceBusy`.
- `try_acquire_workspace_different_workspaces_dont_collide` —
  concurrent acquires with different workspace_ids both succeed.
- `release_workspace_unlocks_for_subsequent_acquire` — acquire,
  release, re-acquire same workspace_id → second acquire OK.
- `release_workspace_is_idempotent` — release twice → no panic,
  second release is a no-op.
- `try_acquire_workspace_empty_id_rejected` — empty
  `workspace_id` → `AppError::InvalidInput`, NOT `WorkspaceBusy`.

#### `coordinator::fsm` tests (no real `claude` spawning)

These tests use a `MockTransport` trait abstraction. Refactor
`SubprocessTransport::invoke` into a trait `Transport::invoke`
implemented by both `SubprocessTransport` (production) and
`MockTransport` (tests). The trait is small (one async method);
mock returns canned `InvokeResult`s keyed by `profile.id`.

- `fsm_happy_path_walks_three_stages` — mock returns scripted
  results for scout/planner/backend-builder; assert
  `JobOutcome.final_state == Done`, `stages.len() == 3`,
  `total_cost_usd > 0`.
- `fsm_scout_failure_short_circuits` — mock errors on scout;
  assert `final_state == Failed`, `last_error` populated, no
  PLAN or BUILD stage in `stages`.
- `fsm_planner_failure_short_circuits` — same but mock errors
  on planner; assert one stage (scout) in `stages`, then Failed.
- `fsm_builder_failure_returns_partial_stages` — mock errors on
  backend-builder; assert two stages (scout, planner) in
  `stages`, then Failed with builder error in `last_error`.
- `fsm_records_per_stage_duration` — mock injects a `Duration`
  via a sleep; assert `StageResult.duration_ms` is in the right
  ballpark (>0, <2000).
- `fsm_aggregates_total_cost` — mock returns 0.01, 0.02, 0.03
  per stage; `outcome.total_cost_usd ≈ 0.06`.
- `fsm_unreachable_states_panic_in_debug` — assert that calling
  the internal `next_state(Review)` triggers `debug_assert!`
  failure (only in debug builds; `#[cfg(debug_assertions)]`).
- `prompt_template_scout_passes_goal_verbatim` — given goal
  "X", scout prompt to mock is exactly "X" (no wrapping).
- `prompt_template_plan_includes_scout_findings` — given scout
  output Y, planner prompt contains Y.
- `prompt_template_build_includes_plan_step1_directive` —
  builder prompt contains "ŞU ANDA SADECE ADIM 1'İ UYGULA".

#### Integration test (`#[ignore]`)

- `swarm::coordinator::fsm::tests::integration_fsm_drives_real_claude_chain`
  — same shape as W3-11's `integration_smoke_invoke` but spawns
  the real `claude` for all three stages and asserts
  `final_state == Done`. Goal is the same as the manual
  validation: "Add a `profile_count(&self) -> usize` helper to
  `ProfileRegistry` in `src-tauri/src/swarm/profile.rs`".
  Time budget: **3 × 60s = 180s total**. `#[ignore]` because CI
  has neither `claude` nor OAuth.

#### Refactor — extract Transport trait

W3-11 shipped `SubprocessTransport::invoke` as a free function
on a unit struct. To enable FSM mocking, refactor into:

```rust
pub trait Transport: Send + Sync {
    async fn invoke(&self, ...) -> Result<InvokeResult, AppError>;
}

pub struct SubprocessTransport { /* ... */ }
impl Transport for SubprocessTransport { /* same body as W3-11 */ }
```

This is the only externally-visible breaking change to the
W3-11 surface. `swarm:test_invoke` keeps working — its caller
(commands/swarm.rs) constructs a `SubprocessTransport` directly.

### 7. Bindings

```bash
pnpm gen:bindings
```

Expect new entries:
- `commands.swarmRunJob(goal) -> Promise<JobOutcome>`
- New types: `Job`, `JobState`, `JobOutcome`, `StageResult`

`pnpm gen:bindings:check` exits 0 post-commit.

## Out of scope

- ❌ DB persistence (W3-12b)
- ❌ Streaming Tauri events / frontend hook (W3-12c)
- ❌ REVIEW / TEST states (W3-12d — requires reviewer +
  integration-tester profile authoring)
- ❌ Verdict schema + JSON parser (W3-12d)
- ❌ Retry feedback loop (W3-12d — `MAX_RETRIES` constant lands
  in 12a but no retry consumer)
- ❌ Coordinator LLM brain (W3-12d — Option B)
- ❌ Persistent Coordinator chat (W3-13+ — Option C)
- ❌ Cancel mid-job (W3-12c — needs streaming protocol first)
- ❌ Multi-step BUILD (FSM iterates only once per stage in 12a)
- ❌ Per-job stage-timeout overrides (FSM-wide budget only)

## Acceptance criteria

- [ ] `src-tauri/src/swarm/coordinator/{mod,fsm,job}.rs` exist;
      module declared in `swarm/mod.rs`
- [ ] `swarm:run_job(workspace_id, goal)` IPC compiles and types end-to-end
- [ ] Per-workspace lock honored: same `workspace_id` second call
      → `Err(AppError::WorkspaceBusy{..})`; different `workspace_id`s
      → both run to completion in parallel
- [ ] Empty-string `workspace_id` rejected with `AppError::InvalidInput`
- [ ] `JobRegistry` is `app.manage`d in `lib.rs::setup`
- [ ] FSM unit tests (≥10) pass against `MockTransport`; integration
      test (`#[ignore]`d) compiles
- [ ] `Transport` trait extracted; `SubprocessTransport` (W3-11)
      refactored to implement it; `swarm:test_invoke` keeps
      working
- [ ] `MAX_RETRIES = 2` const exported from `coordinator::fsm`
- [ ] `JobState::{Review, Test}` variants defined but unreachable
      (debug_assert guard)
- [ ] No new `unsafe`, no `eprintln!`, no new dep
- [ ] All Week-2 + Week-3 prior tests still pass (regression: 181
      + new tests; target ≥193)
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check`
      exits 0 post-commit
- [ ] **Owner-driven manual integration smoke** (post-commit):
      `cargo test -- integration_fsm_drives_real_claude_chain
      --ignored --nocapture` against a logged-in `claude`;
      finishes with `Done` state in <180s for the canonical
      `profile_count` goal

## Verification commands

```bash
# Rust gates
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Bindings
pnpm gen:bindings
pnpm gen:bindings:check

# Frontend
pnpm typecheck
pnpm test --run
pnpm lint

# Manual integration smoke (owner-driven, post-commit)
cargo test --manifest-path src-tauri/Cargo.toml --lib \
    -- swarm::coordinator::fsm::tests::integration_fsm_drives_real_claude_chain \
    --ignored --nocapture
```

## Notes / risks

- **`Transport` trait refactor surface**. W3-11's
  `SubprocessTransport::invoke` becomes `<dyn Transport>::invoke`.
  Existing `swarm::transport::tests` still pass against the
  `SubprocessTransport` impl directly. Mock impl lives under
  `#[cfg(test)] mod mock_transport` to avoid leaking test-only
  types into the public surface.
- **Stage timeout granularity**. 60s/stage may not be enough for
  long Builder calls; the env var override
  (`NEURON_SWARM_STAGE_TIMEOUT_SEC`) is intentionally a global,
  not per-stage, knob — stage-specific budgets are W3-12d
  territory once Verdict / retry logic exists.
- **In-memory `JobRegistry` lifecycle**. App restart loses every
  in-flight job. W3-12b replaces this with a SQLite-backed
  registry on the same trait surface; W3-12a's `JobRegistry`
  becomes one of two impls. Tests can use the in-memory one
  unchanged.
- **Concurrent `swarm:run_job` calls.** Two parallel jobs with
  **different** `workspace_id`s share the registry but never the
  same `job_id`. Each `run_job` holds its own `Job` and writes
  through the registry's per-key lock. **Same** `workspace_id`
  → second call rejected with `WorkspaceBusy` per owner directive
  2026-05-05. No global FSM mutex.
- **Profile loading per call**. `ProfileRegistry::load_from` is
  called fresh on every `swarm:run_job`. For a 3-stage job with
  the bundled-only set this is ~1ms — negligible. W3-12b/c may
  introduce a startup-cached registry; not in 12a.
- **Failure states leak into `JobOutcome`.** `final_state == Failed`
  + `last_error` is the only failure surface — there is no
  `AppError::SwarmJob` variant. The IPC contract is "the IPC
  always returns Ok unless the FSM itself can't even start"; the
  business-failure surface is `JobOutcome.final_state`.
- **No streaming means user blocks**. `swarm:run_job` may run
  for 30–180s. Frontend must show a "running…" UI — but the
  frontend hook for that is W3-12c. Pre-12c usage is via
  DevTools/curl-style invocations.

## Sub-agent reminders

- Read this WP in full before writing code. The transition table
  and prompt templates are the contract.
- Read `WP-W3-11-swarm-foundation.md` and the existing
  `src-tauri/src/swarm/{binding,profile,transport}.rs` for the
  `Command` / `BufReader` / error-mapping patterns to mirror.
- The architectural report `report/Neuron Multi-Agent
  Orchestration` §5 (Coordinator State Machine) and §11.4
  (Single Claude Instance vs Multiple Processes) explain why
  Option A — pure Rust FSM, no Coordinator LLM — is the right
  W3-12a choice.
- DO NOT introduce a Coordinator LLM in 12a. That is W3-12d.
- DO NOT add DB writes. That is W3-12b.
- DO NOT add Tauri events / streaming. That is W3-12c.
- DO NOT add new specialist profiles. The bundled three are
  the contract.
- DO NOT change `swarm:test_invoke` semantics — refactoring
  `Transport` into a trait is allowed, but the existing IPC
  surface (`swarmTestInvoke`) and its `InvokeResult` shape stay
  identical.
- DO NOT add a new dep without justification. The FSM is
  std-only Rust + tokio (already in tree).
- DO NOT split into multiple commits — per `AGENTS.md`, one WP =
  one commit, the orchestrator handles atomicity.

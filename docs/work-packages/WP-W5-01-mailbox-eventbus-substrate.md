---
id: WP-W5-01
title: Mailbox event-bus substrate (kind / parent_id / payload_json columns + workspace broadcast channel)
owner: TBD
status: not-started
depends-on: []
acceptance-gate: "Migration `0010_mailbox_eventbus.sql` adds `kind`, `parent_id`, `payload_json` columns to the `mailbox` table without breaking existing rows. New `MailboxBus` service in `src-tauri/src/swarm/mailbox_bus.rs` exposes per-workspace `tokio::sync::broadcast` channels. New `mailbox:emit_typed` and `mailbox:list_typed` IPCs land alongside the existing `mailbox:emit` / `mailbox:list`. Specta-typed `MailboxEvent` enum covers `task_dispatch / agent_result / agent_help_request / coordinator_help_outcome / job_started / job_finished / job_cancel / note`. NO behavior change to FSM, registry, help-loop, or any existing IPC. `cargo test --lib` green; ≥ 12 new unit tests covering migration round-trip, broadcast fan-out, parser variants, and back-compat with existing mailbox rows. `pnpm typecheck` / `lint` / `gen:bindings:check` green."
---

## Goal

Lay the substrate for W5's autonomous mailbox-driven swarm. The
existing `mailbox` table (W2-02) is already a generic event log; W5-01
extends it with the structured fields a message-bus needs (kind,
parent_id, payload_json) and adds an in-process broadcast primitive
(`tokio::sync::broadcast` per workspace) for wake-on-message latency.

This WP is **pure substrate** — no FSM change, no registry change, no
help-loop change. Existing callers of `mailbox_emit` /
`mailbox::emit_internal` keep working unchanged. The new surface is
*additive*. The W5-02 sub-WP wires agents to the broadcast; this WP
just makes the broadcast available.

## Why now

Owner directive 2026-05-09: relax the deterministic FSM into a
fully autonomous mailbox-driven swarm. W5-01 is the foundational
piece — every other W5 sub-WP (agent subscription, Coordinator
brain, job-state derivation, cancel migration, FSM teardown)
builds on the typed `MailboxEvent` shape and the broadcast
primitive defined here.

## Charter alignment

- **Tech stack**: no new dependency. `tokio::sync::broadcast` is
  already in the dep tree (used by tracing-subscriber transitively;
  re-exporting from `tokio` is free).
- **Frontend mock shape**: the existing `MailboxEntry` wire shape
  (`from`/`to`/`type`/`summary` per Charter Constraint #1) is
  preserved. The new typed surface is a *parallel* IPC, not a
  replacement. Frontend mock parity is unaffected.
- **Identifier strategy** (ADR-0007): mailbox stays
  autoincrement-integer per ADR-0007 §3. `parent_id` references
  another mailbox row's autoincrement id. No prefixed-ULID involved.
- **Timestamp invariant** (Charter §8): `ts` stays unix-epoch
  *seconds* (matching existing column). No `_ms` suffix change.
- **OKLCH-only** (Charter §"Hard constraints" #4): N/A — no UI in
  this WP.

## Scope

### 1. Migration `src-tauri/migrations/0010_mailbox_eventbus.sql`

```sql
-- 0010 — mailbox event-bus extension (WP-W5-01).
--
-- Extends the W2-02 mailbox table with three columns so it can
-- carry structured event-bus payloads alongside the legacy
-- terminal-pane / swarm-help-loop entries:
--
--   * `kind` — event-shape discriminator. Defaults to `'note'` for
--     legacy rows and free-form mailbox uses; W5-driven emitters
--     set it to one of the structured variants (`task.dispatch`,
--     `agent.result`, `agent.help_request`,
--     `coordinator.help_outcome`, `job.started`, `job.finished`,
--     `job.cancel`). The dot-separated string is the SQL form;
--     wire form (Tauri event names) keeps the colon substitution
--     per ADR-0006 — but `kind` is a *payload field*, not an event
--     name, so the dot is fine.
--
--   * `parent_id` — reply-to / correlation reference. Points at
--     another mailbox row's `id` (the autoincrement PK from
--     migration 0002). Used by W5-02+ to chain `agent.result` →
--     `task.dispatch` and by W5-04 to derive retry attempts.
--     Nullable (top-level events like `job.started` have no parent).
--     No FK constraint — mailbox rows are append-only and never
--     deleted, so referential integrity is upheld by the emit path.
--     A FK would force ON DELETE CASCADE / RESTRICT decisions that
--     don't fit the append-only contract.
--
--   * `payload_json` — typed body, defaults to `'{}'`. Lets W5-driven
--     emitters carry structured payloads (e.g. dispatch prompt,
--     agent result with cost/turn-count, verdict JSON) without
--     squeezing them through `summary`. Legacy `summary`-only
--     callers default to `'{}'`.
--
-- ALTER TABLE on SQLite is restricted; ADD COLUMN with a default
-- is the only safe op (matches the WP-W3-12d / WP-W3-12f pattern).
-- Backfill is implicit — every existing row gets the defaults.
--
-- Index on `(workspace_id_resolved, kind, id)` is NOT added here.
-- The current `idx_mailbox_ts` is sufficient for the
-- `mailbox:list_typed` query patterns in W5-01 (kind filter +
-- since_id paging). If a future W5 WP shows hot-path scans on
-- (kind, since_id), add the composite index then. Avoid premature
-- indexing per Charter §"Don't add features beyond what the task
-- requires".

ALTER TABLE mailbox ADD COLUMN kind         TEXT NOT NULL DEFAULT 'note';
ALTER TABLE mailbox ADD COLUMN parent_id    INTEGER;
ALTER TABLE mailbox ADD COLUMN payload_json TEXT NOT NULL DEFAULT '{}';
```

Migration version bumped accordingly in `src-tauri/src/db.rs`'s
expected migration count check (currently `expected = 9`, becomes
`10`) AND the table-count assertion if it counts mailbox columns
(verify by reading `db.rs` first; the W4-07 commit didn't add a
new table so the table count likely stayed at 16 — confirm).

### 2. New module `src-tauri/src/swarm/mailbox_bus.rs`

```rust
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, Runtime};
use tokio::sync::{broadcast, RwLock};

use crate::db::DbPool;
use crate::error::AppError;
use crate::events;
use crate::models::MailboxEntry;
use crate::time::now_seconds;

/// Capacity of each per-workspace broadcast channel. `64` is well
/// past the burst rate of any single dispatch (a single Coordinator
/// brain turn produces at most O(10) events: dispatch + result +
/// optional help round-trip). Receivers that lag past `64` get a
/// `RecvError::Lagged` and skip ahead — acceptable since the
/// SQLite log is the source of truth; consumers can recover via
/// `mailbox:list_typed(since_id)` after a lag.
const BROADCAST_CAPACITY: usize = 64;

/// Structured event-bus payload. Discriminated by `kind` field on
/// the wire (snake_case). The variant body carries the typed
/// payload; the SQL row's `payload_json` column persists the same
/// shape verbatim so a process restart can rebuild events from
/// SQLite without losing fidelity.
///
/// Variant selection matches the W5-overview table:
/// `task.dispatch / agent.result / agent.help_request /
/// coordinator.help_outcome / job.started / job.finished /
/// job.cancel / note`.
#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MailboxEvent {
    /// W5-03: Coordinator brain dispatches a task to a specific
    /// agent. `target` is `agent:<id>` per the W4-07 namespacing.
    /// `prompt` is the user-message fed into the agent's session.
    /// `with_help_loop` toggles the W4-05 help-loop on the dispatch;
    /// defaults to true for builders/scout/planner, false for
    /// reviewers/tester whose persona contracts forbid help blocks.
    TaskDispatch {
        job_id: String,
        target: String,
        prompt: String,
        with_help_loop: bool,
    },
    /// W5-02: agent emitted result for a dispatch. `parent_id`
    /// (carried in the row's `parent_id` column, NOT here) points
    /// at the originating `TaskDispatch` row.
    AgentResult {
        job_id: String,
        agent_id: String,
        assistant_text: String,
        total_cost_usd: f64,
        turn_count: u32,
    },
    /// W5-02: agent emitted a `neuron_help` block via W4-05's parser.
    AgentHelpRequest {
        job_id: String,
        agent_id: String,
        reason: String,
        question: String,
    },
    /// W5-03: Coordinator's response to a help request. Mirrors
    /// `CoordinatorHelpOutcome` from `swarm::help_request`.
    CoordinatorHelpOutcome {
        job_id: String,
        target_agent_id: String,
        outcome: serde_json::Value,
    },
    /// W5-03: job lifecycle start. Emitted once per job by the
    /// `swarm:run_job_v2` IPC; CoordinatorBrain subscribes and
    /// drives the dispatch loop.
    JobStarted {
        job_id: String,
        workspace_id: String,
        goal: String,
    },
    /// W5-03: job lifecycle finish. Emitted by CoordinatorBrain
    /// when the brain returns a `finish` action.
    JobFinished {
        job_id: String,
        outcome: String, // "done" | "failed"
        summary: String,
    },
    /// W5-05: cancel signal. CoordinatorBrain + agent dispatchers
    /// subscribe; in-flight turns truncate.
    JobCancel { job_id: String },
    /// Legacy free-form note. Default kind for back-compat
    /// emitters (the existing `mailbox::emit_internal` /
    /// `mailbox_emit` IPCs keep emitting `kind='note'`).
    Note,
}

impl MailboxEvent {
    /// Sql `kind` string for this variant. Stable; matches the
    /// migration's column values verbatim (with dot separators).
    pub fn kind_str(&self) -> &'static str {
        match self {
            MailboxEvent::TaskDispatch { .. } => "task.dispatch",
            MailboxEvent::AgentResult { .. } => "agent.result",
            MailboxEvent::AgentHelpRequest { .. } => "agent.help_request",
            MailboxEvent::CoordinatorHelpOutcome { .. } => {
                "coordinator.help_outcome"
            }
            MailboxEvent::JobStarted { .. } => "job.started",
            MailboxEvent::JobFinished { .. } => "job.finished",
            MailboxEvent::JobCancel { .. } => "job.cancel",
            MailboxEvent::Note => "note",
        }
    }

    /// Reverse of `kind_str` + JSON parse. Used by the projector
    /// + `mailbox:list_typed` to rebuild events from SQLite rows.
    pub fn from_row_parts(
        kind: &str,
        payload_json: &str,
    ) -> Result<Self, AppError> {
        // Prepend the kind discriminator if missing, then deserialize
        // the tagged-enum form. Defense-in-depth: if `payload_json`
        // already includes the kind tag, we deserialize directly.
        // ... (implementation: try direct deserialize first; on miss,
        // splice `kind` into a JSON object wrapper).
        unimplemented!("see test fixtures for round-trip examples")
    }
}

/// Shape of one persisted row, decorated with the typed event.
/// Returned by `MailboxBus::list_typed`. The `id`, `ts`, `from`,
/// `to`, `summary` mirror `MailboxEntry`'s wire shape so the
/// frontend can render either type with the same code path.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct MailboxEnvelope {
    pub id: i64,
    pub ts: i64,
    #[serde(rename = "from")]
    pub from_pane: String,
    #[serde(rename = "to")]
    pub to_pane: String,
    pub summary: String,
    pub parent_id: Option<i64>,
    pub event: MailboxEvent,
}

/// Per-workspace pubsub. Held in `app.manage(...)` next to
/// `SwarmAgentRegistry` (W4-02) and the `DbPool`. Lazy-creates
/// the broadcast channel for a workspace on first
/// `subscribe` / `publish` call.
pub struct MailboxBus {
    pool: DbPool,
    channels: RwLock<HashMap<String, broadcast::Sender<MailboxEnvelope>>>,
}

impl MailboxBus {
    pub fn new(pool: DbPool) -> Self { /* ... */ }

    /// Get a receiver for the named workspace. Creates the channel
    /// on first call. Subsequent subscribers share the same channel.
    pub async fn subscribe(
        &self,
        workspace_id: &str,
    ) -> broadcast::Receiver<MailboxEnvelope> { /* ... */ }

    /// Persist + broadcast + Tauri-emit one event. Atomic to the
    /// extent SQLite + Tauri allow:
    /// 1. INSERT row (autoincrement id).
    /// 2. Build envelope from row + event.
    /// 3. `broadcast::Sender::send` to in-process subscribers.
    /// 4. `app.emit("mailbox:new", envelope.legacy_form())` for
    ///    Tauri-side listeners (frontend mailbox panel).
    ///
    /// Any failure rolls back: SQL error → return Err, no broadcast.
    /// Broadcast send error (no receivers) is silently swallowed —
    /// agents may not be subscribed yet.
    ///
    /// `from_pane` / `to_pane` are caller-provided strings using
    /// the W4-07 namespacing convention (`agent:<id>` for swarm
    /// rows, `pane:<uuid>` for terminal-pane rows). The bus does
    /// not enforce a format; convention is documented in the WP.
    pub async fn emit_typed<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        workspace_id: &str,
        from_pane: &str,
        to_pane: &str,
        summary: &str,
        parent_id: Option<i64>,
        event: MailboxEvent,
    ) -> Result<MailboxEnvelope, AppError> { /* ... */ }

    /// Read events for a workspace + optional kind filter +
    /// optional `since_id` cursor. Returns oldest-first so the
    /// projector (W5-04) can replay events in order. Defaults to
    /// `since_id = 0` (all rows for the workspace).
    ///
    /// **Workspace scoping**: the existing mailbox schema has no
    /// `workspace_id` column. W5-01 derives workspace from the
    /// W4-07 `from_pane`/`to_pane` `agent:<id>` namespace —
    /// agent ids are workspace-scoped via the registry, so an
    /// agent_id implies a workspace. For a multi-workspace future
    /// (post-W5), a column will be needed; for W5 single-workspace,
    /// the bus filters by passing the agent ids it knows belong to
    /// the workspace. Concretely: the bus's per-workspace channel
    /// fans out everything it sees to that workspace's subscribers,
    /// and `list_typed` filters by `kind` only (workspace filtering
    /// is moot for single-workspace W5).
    pub async fn list_typed(
        &self,
        kind: Option<&str>,
        since_id: Option<i64>,
        limit: Option<u32>,
    ) -> Result<Vec<MailboxEnvelope>, AppError> { /* ... */ }
}
```

### 3. New IPCs in `src-tauri/src/commands/mailbox.rs`

```rust
/// W5-01 — typed emit for the event-bus. Mirrors `mailbox_emit`
/// but takes a structured `MailboxEvent` and routes through the
/// `MailboxBus` for both persistence + broadcast.
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_emit_typed<R: Runtime>(
    app: AppHandle<R>,
    bus: State<'_, Arc<MailboxBus>>,
    workspace_id: String,
    from_pane: String,
    to_pane: String,
    summary: String,
    parent_id: Option<i64>,
    event: MailboxEvent,
) -> Result<MailboxEnvelope, AppError> { /* ... */ }

/// W5-01 — typed list with kind filter + since-id cursor. Used by
/// the projector (W5-04) to replay events on mount and by the
/// "Swarm comms" tab UI (post-W5).
#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn mailbox_list_typed(
    bus: State<'_, Arc<MailboxBus>>,
    kind: Option<String>,
    since_id: Option<i64>,
    limit: Option<u32>,
) -> Result<Vec<MailboxEnvelope>, AppError> { /* ... */ }
```

`limit` defaults to 100, capped at 500 (mirrors
`SWARM_LIST_JOBS_DEFAULT_LIMIT` / `_MAX_LIMIT` shape).
Validate inputs identically to the existing IPCs (empty strings
rejected with `AppError::InvalidInput`).

### 4. Wire-up in `src-tauri/src/lib.rs`

- After `app.manage(swarm_agent_registry)`, add
  `app.manage(Arc::new(MailboxBus::new(pool.clone())))`.
- Register `mailbox_emit_typed` + `mailbox_list_typed` in the
  specta + tauri-specta builder lists alongside `mailbox_emit` /
  `mailbox_list`.
- Register `MailboxEvent`, `MailboxEnvelope` as exported types
  (so they land in `bindings.ts`).

### 5. Module re-exports

- `src-tauri/src/swarm/mod.rs` re-exports `MailboxBus`,
  `MailboxEvent`, `MailboxEnvelope`.

### 6. Bindings regen

`pnpm gen:bindings` produces:
- New `MailboxEvent` tagged-union type (with eight variants).
- New `MailboxEnvelope` interface.
- New `commands.mailboxEmitTyped` / `commands.mailboxListTyped`.

Commit the regenerated `app/src/lib/bindings.ts` in the same
commit as the Rust code (per the W3 / W4 pattern).

## Out of scope (per W5-overview)

- ❌ Agent-side mailbox subscription (W5-02 owns the dispatcher
  task that consumes `MailboxEvent::TaskDispatch` and routes to
  agent sessions). W5-01 ships the bus; W5-02 wires it.
- ❌ Coordinator brain (W5-03 owns the dispatch loop, parser, and
  `swarm:run_job_v2` IPC).
- ❌ Job state derivation from mailbox (W5-04 owns the projector).
- ❌ Cancel migration (W5-05 owns the `JobCancel` propagation
  through agents/brain).
- ❌ FSM teardown (W5-06).
- ❌ Workspace_id column on `mailbox` — out of scope for W5
  (single-workspace per Charter §9). Multi-workspace is post-W5.
- ❌ Indexing changes — `idx_mailbox_ts` stays; new composite
  indexes deferred until profiling shows hot-path needs.
- ❌ Frontend hooks for `mailbox:list_typed` — UI consumption
  lands in W5-04.

## Acceptance criteria

A sub-agent must self-verify each box before returning:

- [ ] `cargo build --lib` exits 0
- [ ] `cargo test --lib` exits 0; total count ≥ **447** (baseline
      was 435 after W4; W5-01 adds ≥ 12 unit tests as listed below)
- [ ] `cargo check --all-targets` exits 0
- [ ] `pnpm gen:bindings` regenerates `app/src/lib/bindings.ts`;
      diff is checked into the commit
- [ ] `pnpm gen:bindings:check` exits 0 (post-commit)
- [ ] `pnpm typecheck` exits 0
- [ ] `pnpm lint` exits 0
- [ ] `pnpm test --run` exits 0 (frontend test count unchanged at 65;
      this WP has no frontend test additions)
- [ ] All tests listed below exist and pass:
  - [ ] `migration_0010_round_trip` — apply + assert columns exist
        + insert row with defaults + read back
  - [ ] `mailbox_event_kind_str_round_trip` — every variant's
        `kind_str()` output round-trips through `from_row_parts`
  - [ ] `mailbox_event_from_row_parts_handles_each_variant` — 8
        fixtures, one per `MailboxEvent` variant
  - [ ] `mailbox_event_from_row_parts_rejects_malformed_payload`
        — at least 3 malformed payload cases (invalid JSON, wrong
        shape, missing required field)
  - [ ] `mailbox_bus_subscribe_creates_channel_on_first_call`
  - [ ] `mailbox_bus_subscribe_shares_channel_across_calls`
        — two subscribers see the same emit
  - [ ] `mailbox_bus_emit_persists_row` — SELECT confirms the row
        landed in the table with correct `kind`, `parent_id`,
        `payload_json`
  - [ ] `mailbox_bus_emit_broadcasts_envelope` — subscriber
        receives the envelope after emit
  - [ ] `mailbox_bus_emit_swallows_broadcast_send_error_on_no_subscribers`
        — emit succeeds even when no subscribers attached
  - [ ] `mailbox_bus_emit_fires_legacy_mailbox_new_event` — back-compat:
        the `mailbox:new` Tauri event still fires with the legacy
        `MailboxEntry` shape, so existing frontend listeners keep
        working
  - [ ] `mailbox_emit_typed_validates_empty_inputs` — rejects empty
        workspace_id / from / to with `AppError::InvalidInput`
  - [ ] `mailbox_list_typed_filters_by_kind` — fixture with mixed
        kinds returns only the filtered rows
- [ ] `mailbox_list` (legacy IPC) keeps working unchanged — assert
      via existing `mailbox_list_*` tests passing
- [ ] `mailbox_emit` (legacy IPC) keeps working unchanged —
      assert via existing `mailbox_emit_*` tests passing; the
      legacy emit path now writes `kind='note'`, `parent_id=NULL`,
      `payload_json='{}'` implicitly via the column defaults
- [ ] No FSM, registry, help-loop, or `swarm:run_job` behavior
      changes (verify by running real-claude smokes optionally;
      not required for this WP since none of those code paths are
      touched)

## Verification commands

Run in this exact order before returning:

```powershell
# Rust
cd src-tauri
cargo build --lib
cargo test --lib
cargo check --all-targets
cd ..

# Bindings + frontend
pnpm gen:bindings
git add app/src/lib/bindings.ts   # pre-commit; sub-agent commits everything together
pnpm gen:bindings:check           # exit 0 post-stage
pnpm typecheck
pnpm lint
pnpm test --run
```

`cargo test` is expected to take 60-120s (no slow integration
tests in this WP). `pnpm test` is expected at < 30s.

## Files allowed to modify

The sub-agent MAY create/edit:

- `src-tauri/migrations/0010_mailbox_eventbus.sql` (new)
- `src-tauri/src/swarm/mailbox_bus.rs` (new)
- `src-tauri/src/swarm/mod.rs` (re-exports only)
- `src-tauri/src/commands/mailbox.rs` (add 2 new IPCs; keep
  existing IPCs verbatim)
- `src-tauri/src/lib.rs` (specta registration + `app.manage`)
- `src-tauri/src/db.rs` (migration count assertion bump from 9 → 10)
- `app/src/lib/bindings.ts` (regenerated only — never hand-edited)
- `docs/work-packages/WP-W5-01-mailbox-eventbus-substrate.md`
  (this file — to flip `status: not-started` → `status: done`
  on completion + add a "Result" section)
- `AGENT_LOG.md` (append entry per AGENTS.md template)

The sub-agent MUST NOT touch:

- Any file in `src-tauri/src/swarm/coordinator/` (FSM stays put)
- `src-tauri/src/swarm/agent_registry.rs` (W5-02 owns the
  registry-side wiring)
- `src-tauri/src/swarm/help_request.rs` (help-loop unchanged)
- `src-tauri/src/swarm/transport.rs` /
  `src-tauri/src/swarm/persistent_session.rs` (transport stays
  one-shot/persistent split)
- Any frontend component (no UI in this WP)
- Any persona file in `src-tauri/src/swarm/agents/*.md`

## Notes / risks

- **Migration backward-compat**: existing rows lack `kind` /
  `parent_id` / `payload_json`. ALTER TABLE with DEFAULT clauses
  fills them in for existing rows on the migration's apply path.
  Verify by running the migration against a pre-W5 database
  fixture in `migration_0010_round_trip`.
- **Specta tagged-enum**: `#[serde(tag = "kind", rename_all =
  "snake_case")]` produces a TypeScript discriminated union with
  `kind: "task_dispatch" | "agent_result" | …`. Note: the SQL
  `kind` column uses dot-separated names (`task.dispatch`) for
  human readability in the DB; the wire form uses underscore
  (`task_dispatch`) per Tauri/specta convention. The
  `MailboxEvent::kind_str()` returns the SQL form; the serde
  tag uses the wire form. They are NOT interchangeable —
  document this clearly in the module doc comment.
- **Broadcast capacity 64**: a single Coordinator turn produces ~3
  events. 64 is well past any realistic burst. If a future smoke
  shows `RecvError::Lagged` firing under stress, bump the
  capacity (it's a `const`, not a public knob — don't expose it
  as an env override unless profiling demands).
- **`broadcast::Sender::send` error semantics**: `send` returns
  `Err(SendError)` only when no receivers are attached. We
  silently swallow this case — agents subscribe lazily, and
  emitting `JobStarted` before any agent is subscribed must not
  fail. Persist-success without broadcast-success is the correct
  semantics: the SQL log is the source of truth, broadcast is
  the wake-up optimization.
- **Workspace scoping under single-workspace W5**: the bus's
  per-workspace map is real (one channel per workspace), but in
  practice every running install has one workspace ("default").
  The map is future-proofed; the W5-02+ code is correct on
  multi-workspace day-zero.
- **Test isolation**: every test that uses `MailboxBus` should
  create a fresh `mock_app_with_pool` fixture and a fresh
  `MailboxBus` so broadcast state doesn't leak across tests.
  Existing `mailbox_*` tests use this pattern; reuse the
  `test_support::mock_app_with_pool` helper.
- **No `_at` / `_ms` field changes**: the migration adds a
  `payload_json` column whose contents may include timestamps,
  but the column name doesn't end in `_at` / `_ms` so Charter §8
  is N/A. Inside `payload_json`, callers MUST preserve the
  invariant if their event variant carries timestamps — but
  W5-01's variants don't currently include any (timestamps
  surface via `MailboxEnvelope.ts`, the row's own column).

## Sub-agent prompt template

When the orchestrator dispatches a sub-agent for this WP, the
prompt should include:

1. The full text of this file (verbatim paste)
2. A pointer to `PROJECT_CHARTER.md` for scope conflicts
3. The "Verification commands" block as the self-verify gate
4. The "Files allowed to modify" list as the scope boundary
5. The "Acceptance criteria" checklist
6. AGENTS.md reminder: "no `--no-verify` commits; conventional
   commit message; co-author line per AGENTS.md §Commits"
7. Reminder: "do NOT change frontend mock shape; do NOT add a
   build step beyond what this WP authorizes; do NOT introduce
   technologies outside Charter's tech-stack table"

The sub-agent returns a summary covering:

- Files changed (count + key paths)
- Test count delta (baseline → new)
- Acceptance: pass / fail-with-detail
- Commit SHA
- Any unexpected scope expansions (with rationale)

## Result

(Filled in by the sub-agent on completion. Sections:
`Implementation summary`, `Test count delta`, `Bindings regen`,
`Commits`, `Caveats`.)

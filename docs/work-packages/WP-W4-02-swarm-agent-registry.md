---
id: WP-W4-02
title: Workspace-scoped `SwarmAgentRegistry` + lazy spawn lifecycle
owner: TBD
status: not-started
depends-on: [WP-W4-01]
acceptance-gate: "New `SwarmAgentRegistry` in `src-tauri/src/swarm/agent_registry.rs` keyed by `(workspace_id, agent_id)` holding `PersistentSession`s + per-agent metadata. Lazy spawn rules implemented: Orchestrator on first `swarm:orchestrator_decide` for the workspace; Coordinator + 7 specialists on first `Dispatch` outcome. Eager kill on workspace close (new `swarm:agents:shutdown_workspace(workspaceId)` IPC). Per-agent status (`Idle / Spawning / Running / WaitingOnCoordinator / Blocked / Crashed`). Turn-cap respawn under `NEURON_SWARM_AGENT_TURN_CAP` (default 200). New IPC `swarm:agents:list_status(workspaceId)` for the eventual W4-04 grid header. `cargo test --lib` green (≥ 12 new tests)."
---

## Goal

Own the lifecycle of the W4-01 `PersistentSession`s. Today (post-
W4-01) we have a multi-turn transport but nobody to spawn or kill
sessions, decide when to spawn them, or track status. W4-02 is that
owner.

## Why now

The owner directive 2026-05-07 §1B (persistent sessions live as
long as the workspace) needs an actor. Without W4-02 every
`swarm:orchestrator_decide` and `swarm:run_job` would still
spawn-and-kill subprocesses on every call — regressing all the
W4-01 wins. With W4-02 the registry hands out an existing session
or lazy-spawns a fresh one, so the second-turn-and-onward latency
collapses to "just the LLM".

## Charter alignment

No tech-stack change. New Rust module + Tauri-managed state +
two new IPC commands (one read-only status query + one
admin-style workspace shutdown). No frontend in W4-02 (that's
W4-03 + W4-04).

## Scope

### 1. New module `src-tauri/src/swarm/agent_registry.rs`

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Runtime};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::error::AppError;
use crate::swarm::persistent_session::PersistentSession;
use crate::swarm::profile::ProfileRegistry;

/// Per-agent status visible to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Not yet spawned. Default for every (workspace, agent) pair
    /// before the first lazy-spawn fires.
    NotSpawned,
    /// Spawning is in flight. Brief — only visible across one
    /// `try_spawn_*` window. The status flips to `Idle` once the
    /// session is in the registry.
    Spawning,
    /// Session ready, no turn in flight.
    Idle,
    /// `invoke_turn` is in flight against this session.
    Running,
    /// Specialist emitted a `neuron_help` block (W4-05) and is
    /// awaiting the Coordinator's reply. The session is alive but
    /// not consuming model turns.
    WaitingOnCoordinator,
    /// The session crashed (subprocess died unrecoverably). Will
    /// be respawned on next `acquire_for_turn`.
    Crashed,
}

/// Wire shape for `swarm:agents:list_status`. Mirrors the
/// in-memory metadata the registry holds; trimmed to what the UI
/// actually renders.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusRow {
    pub workspace_id: String,
    pub agent_id: String,
    pub status: AgentStatus,
    pub turns_taken: u32,
    /// Wall-clock ms since UNIX epoch of the most recent
    /// state-changing event (spawn, turn start, turn end, crash).
    /// `None` if the agent has never been touched.
    pub last_activity_ms: Option<i64>,
}

/// Workspace-scoped session registry. Keyed by
/// `(workspace_id, agent_id)`; agent_id is the `Profile.id` from
/// the bundled or workspace-overridden profile registry.
pub struct SwarmAgentRegistry {
    /// Inner map. Outer `RwLock` guards structural changes
    /// (insertions / removals); inner `Mutex` serialises calls
    /// against a single session (no concurrent `invoke_turn` on
    /// the same `PersistentSession` — that's a programming error
    /// per W4-01 docs).
    sessions: RwLock<HashMap<(String, String), Arc<Mutex<AgentSession>>>>,
    /// The profile registry hands out personas. Cloned once at
    /// `new()`; stays read-only for the registry's lifetime.
    profiles: Arc<ProfileRegistry>,
    /// Hard cap on `turns_taken` before the registry triggers a
    /// graceful respawn. Read from `NEURON_SWARM_AGENT_TURN_CAP`
    /// at `new()`, falling back to `DEFAULT_TURN_CAP`.
    turn_cap: u32,
}

struct AgentSession {
    session: PersistentSession,
    status: AgentStatus,
    turns_taken: u32,
    last_activity_ms: Option<i64>,
}

impl SwarmAgentRegistry {
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self;

    /// Acquire (or lazy-spawn) the session for one
    /// (workspace, agent) and run one turn against it. Caller
    /// is the FSM (W4-06) for specialists, the chat IPC for
    /// Orchestrator. Failure paths leave the session in
    /// `Crashed` state; the next call respawns transparently.
    ///
    /// Cancel: forwarded to `PersistentSession::invoke_turn`.
    pub async fn acquire_and_invoke_turn<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> Result<crate::swarm::transport::InvokeResult, AppError>;

    /// Read-only snapshot for `swarm:agents:list_status`. Cheap
    /// (clones the metadata, never the session itself).
    pub async fn list_status(
        &self,
        workspace_id: &str,
    ) -> Vec<AgentStatusRow>;

    /// Eager shutdown — calls `shutdown()` on every session for
    /// `workspace_id`. Idempotent. Used by
    /// `swarm:agents:shutdown_workspace` and by the lib.rs
    /// teardown path on app close.
    pub async fn shutdown_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<(), AppError>;

    /// Eager shutdown of ALL workspaces — called from the
    /// `tauri::App::on_window_event` `CloseRequested` branch (or
    /// on graceful shutdown). Iterates every entry, calls
    /// `shutdown()`, drops the map.
    pub async fn shutdown_all(&self) -> Result<(), AppError>;
}

const DEFAULT_TURN_CAP: u32 = 200;
```

The `Arc<Self>` receiver on `acquire_and_invoke_turn` is so the
registry can pass cloned references into spawned tasks for the
W4-03 event channel emission. (No spawn in W4-02; the signature
is forward-compat.)

### 2. Lazy spawn rules

**Orchestrator agent** (`agent_id == "orchestrator"`):
- Spawned by `swarm:orchestrator_decide` on the first call for
  the workspace.
- Lifetime: until `swarm:agents:shutdown_workspace` OR the
  `turn_cap` triggers a graceful respawn.

**Coordinator + 7 specialists** (every other `agent_id`):
- Spawned by the FSM (W4-06) on the first `Dispatch` outcome for
  the workspace. The FSM calls `acquire_and_invoke_turn` for the
  Coordinator brain first; that triggers the lazy spawn for the
  Coordinator, and as the FSM walks through Scout/Plan/Build/...
  each specialist is spawned on its first turn.
- All eight stay alive across multiple jobs in the same
  workspace — the user might run a second job after the first
  finishes; we don't want to pay 8 cold-starts again.

**No "spawn all 8 up front" surface.** The W4-02 §"Owner decision"
notes that pre-spawning would burn 8 OAuth-using sessions for
users who only ever chat. Lazy-spawn is the lighter-weight default;
W4-04 grid header shows `NotSpawned` pills for un-touched agents.

### 3. Turn-cap respawn

At the start of each `acquire_and_invoke_turn`:

1. Look up the (workspace, agent) entry.
2. If `turns_taken >= turn_cap`, take the slow path:
   - Capture the existing session's last context (we don't try
     to replay — just log "respawning agent X after N turns" via
     `tracing::info!`).
   - Call `shutdown()` on the existing session.
   - Spawn a fresh session against the same profile.
   - Reset `turns_taken` to 0.
3. Otherwise reuse the existing session.

The slow path runs inline inside the same `acquire` call — the
caller's wait time on the respawn is the same as a cold-start
(~1-3s), accepted as the cost of bounded memory. Transparent to
the FSM / Orchestrator.

### 4. New IPC commands

```rust
// commands/swarm.rs additions

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_list_status<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<Vec<AgentStatusRow>, AppError>;

#[tauri::command(rename_all = "camelCase")]
#[specta::specta]
pub async fn swarm_agents_shutdown_workspace<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
) -> Result<(), AppError>;
```

Both validate `workspace_id.trim().is_empty()` → `InvalidInput`.

### 5. App state wiring (`lib.rs::run::setup`)

After the `JobRegistry` install (W3-12a), build the
`SwarmAgentRegistry`:

```rust
let agent_registry = Arc::new(
    crate::swarm::SwarmAgentRegistry::new(profiles.clone()),
);
app.manage(agent_registry);
```

`profiles` is the same `ProfileRegistry` already loaded for the
job side. The `Arc` is shared so the FSM (W4-06) and the
Orchestrator IPC (`commands::swarm::swarm_orchestrator_decide`)
both pull the registry via `app.state::<Arc<SwarmAgentRegistry>>()`.

**Shutdown hook**: in the same setup, register a
`tauri::WindowEvent::CloseRequested` handler that calls
`agent_registry.shutdown_all().await` before letting the window
close. This is the eager-kill side of the lifecycle contract.

### 6. Tests (≥ 12 new)

Mock the `PersistentSession` for unit tests. The simplest path:
introduce a `SessionLike` trait alias in `agent_registry.rs` so
the registry can be generic over a session impl, then in tests
substitute a `MockSession` that records `invoke_turn` calls
without spawning a real subprocess.

Trait shape (keep minimal):

```rust
#[async_trait::async_trait]
trait SessionLike: Send {
    async fn invoke_turn(...) -> Result<InvokeResult, AppError>;
    async fn shutdown(self) -> Result<(), AppError>;
    fn turns_taken(&self) -> u32;
    fn profile_id(&self) -> &str;
}
```

Wait — the project doesn't use async-trait per Charter (no
new deps without justification). Use the same generic-over-T
pattern the existing `Transport` trait uses (`fn invoke<R: Runtime>`
with `impl Future` return). Sub-agent designs the exact shape
inline.

#### Unit tests:

- `acquire_lazy_spawns_on_first_call` — fresh registry, call
  acquire, assert spawn happened
- `acquire_reuses_existing_session_on_second_call` — assert no
  re-spawn; same `Arc<MockSession>` returned
- `list_status_reports_not_spawned_for_untouched_agents` — fresh
  registry returns 9 rows all `NotSpawned`
- `list_status_flips_through_running_then_idle` — drive a turn,
  assert status transitions
- `acquire_two_agents_in_same_workspace_succeeds` — both
  Orchestrator and Coordinator can be spawned independently
- `acquire_same_agent_in_two_workspaces_isolated` — different
  workspaces never share a session
- `concurrent_acquire_for_different_agents_does_not_block` —
  parallel calls for different agents in the same workspace
  don't serialise
- `concurrent_acquire_for_same_agent_serialises` — the per-agent
  Mutex enforces serial turn-taking; second acquire waits
- `turn_cap_triggers_respawn` — set turn_cap=2 via builder,
  drive 3 turns, assert respawn happened on turn 3
- `turn_cap_env_override_lands` — set
  `NEURON_SWARM_AGENT_TURN_CAP=10`, assert `new()` reads it
- `crashed_session_respawns_on_next_acquire` — mock turn returns
  Err(SwarmInvoke "child crashed"), next acquire spawns fresh
- `shutdown_workspace_kills_all_agents_for_workspace` — multi-
  agent setup, shutdown_workspace, list_status returns
  `NotSpawned` for all
- `shutdown_workspace_leaves_other_workspaces_alone` — workspace
  isolation
- `shutdown_all_kills_everything` — final teardown path
- `acquire_after_shutdown_respawns` — shutdown then acquire
  works without app restart

#### Command-level tests:

- `swarm_agents_list_status_validates_empty_workspace_id`
- `swarm_agents_shutdown_workspace_validates_empty_workspace_id`
- `swarm_agents_list_status_returns_not_spawned_for_fresh_workspace`

## Files touched

- new: `src-tauri/src/swarm/agent_registry.rs` (~700-900 lines
  including tests)
- modified: `src-tauri/src/swarm/mod.rs` — re-export
  `SwarmAgentRegistry`, `AgentStatus`, `AgentStatusRow`
- modified: `src-tauri/src/commands/swarm.rs` — two new IPCs
- modified: `src-tauri/src/lib.rs` — `specta_builder_for_export`
  registers the two new IPC commands; `setup` builds the registry,
  `app.manage`s it, and registers the close-window shutdown hook
- regenerate: `app/src/lib/bindings.ts` (two new commands +
  `AgentStatus` + `AgentStatusRow` types)

## Acceptance gates

1. `cd src-tauri && cargo build --lib` → exit 0
2. `cd src-tauri && cargo test --lib` → green; new test count
   delta ≥ 12 (unit + command), no flakes
3. `cd src-tauri && cargo check --all-targets` → exit 0
4. `pnpm gen:bindings:check` after regeneration → exit 0
5. Existing real-claude smokes still pass — sub-agent runs
   `integration_research_only_real_claude` AND
   `integration_persistent_two_turn_real_claude` as regression
   checks
6. New W4-02 acceptance: a manual smoke driving two
   `acquire_and_invoke_turn` calls (turn 1 + turn 2) reuses the
   same `PersistentSession` (asserted by checking the underlying
   subprocess PID stays the same — added as a new `#[ignore]`'d
   integration smoke `integration_registry_reuses_session`)

## Out of scope (W4-02)

- ❌ Per-agent event channel — W4-03
- ❌ AgentPane / 3×3 grid UI — W4-04
- ❌ neuron_help parser + Coordinator hub messaging — W4-05
- ❌ FSM persistent-transport adapter — W4-06
- ❌ Mailbox swarm tab — W4-07
- ❌ Multi-workspace UI surface — covered by the W3-14 / W3-12k2
  "default workspace" rule; W4-02 is multi-workspace-capable
  internally but the UI only ever uses `default`
- ❌ Cross-app-restart session persistence — sessions are
  in-memory; new app launch = new sessions
- ❌ Snapshot of session context to disk — same reason
- ❌ Pre-spawn-all-9 surface — lazy spawn is the only mode

## Notes / risks

- **Concurrent acquire on the same (workspace, agent)**: serial,
  enforced by per-session `Mutex`. Test the contention path so a
  programming mistake doesn't deadlock silently.
- **Orchestrator session reuse vs. W3-12k2 chat history**:
  W3-12k2 already injects the last-N messages into every
  Orchestrator decide call; that pattern is unchanged. The
  registry simply makes the underlying subprocess persistent so
  the "render history + invoke" flow is faster on the second
  message. The persistence + chat history become orthogonal.
- **FSM integration in W4-06**: this WP doesn't touch the FSM.
  The FSM still uses one-shot `SubprocessTransport` until W4-06
  rewires it. So the registry sits idle for the FSM path right
  after this WP — only Orchestrator decide actually exercises it.
  That's fine; W4-06 is the cutover.
- **App-close shutdown hook**: Tauri's `WindowEvent::CloseRequested`
  fires on user close. We register a closure that blocks on
  `agent_registry.shutdown_all().await` for up to 5s before
  letting the window close. Crashes during shutdown are
  best-effort logged; we don't block the close on a failed
  shutdown.
- **`Arc<Mutex<AgentSession>>` vs. `RwLock<HashMap>>`**: the
  outer `RwLock` is taken for write only on insert/remove; reads
  (which dominate) take the read lock. Per-session `Mutex` keeps
  the hot-path turn calls fast.

## Sub-agent reminders

- Do NOT touch FSM code (`src-tauri/src/swarm/coordinator/`).
  FSM integration is W4-06.
- Do NOT add Tauri event emit calls. Event channel is W4-03.
- Do NOT delete `SubprocessTransport`. The FSM still uses it.
- Reuse W4-01 patterns: same arg builder, same env strip, same
  ULID persona-tmp convention.
- After editing any `.md` under `src-tauri/src/swarm/agents/`,
  touch `src-tauri/src/swarm/profile.rs`. (W4-02 is unlikely to
  need persona edits; flag if you do.)
- Run `pnpm gen:bindings && pnpm gen:bindings:check` before
  declaring done — the two new IPCs add types.
- Final commit message: `feat: WP-W4-02 SwarmAgentRegistry + lazy
  spawn lifecycle`. Co-Authored-By trailer.

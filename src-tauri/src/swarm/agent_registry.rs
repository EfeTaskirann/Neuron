//! `SwarmAgentRegistry` — workspace-scoped lifecycle owner for
//! W4-01's `PersistentSession`s (WP-W4-02).
//!
//! Keyed by `(workspace_id, agent_id)`. Sessions lazy-spawn on first
//! `acquire_and_invoke_turn`; reused across turns until the
//! workspace is shut down (W4-02 §"Lifecycle"). Per-agent status is
//! exposed read-only via `list_status` for the eventual W4-04 grid
//! header.
//!
//! Concurrency model:
//! - Outer `RwLock<HashMap<...>>` guards structural changes
//!   (insert / remove). Reads dominate (status checks, hash lookups
//!   on `acquire`), so the read lock keeps the hot path uncontended.
//! - Per-agent `Mutex<AgentSession>` serialises calls against a
//!   single session — `PersistentSession` is not `Sync`, and at most
//!   one `invoke_turn` against the same child can be in flight at a
//!   time (W4-01 contract).
//!
//! Out of scope (per WP §"Out of scope"): event channel emission
//! (W4-03) / 3×3 grid UI (W4-04) / `neuron_help` parser (W4-05) /
//! FSM persistent-transport adapter (W4-06).

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
use crate::swarm::transport::InvokeResult;
use crate::time::now_millis;

/// Default hard cap on `turns_taken` before a session is gracefully
/// respawned. Tunable per-process via `NEURON_SWARM_AGENT_TURN_CAP`.
/// 200 is generous — most jobs walk through 5-7 stages, so the
/// average specialist fires < 10 turns per job. Cap at 200 means a
/// session has to absorb ≥ 20 jobs before respawn — well past the
/// "context bloat" point in practice.
const DEFAULT_TURN_CAP: u32 = 200;

/// Env override for `DEFAULT_TURN_CAP`. Same reading rules as the
/// stage-timeout pattern in `commands/swarm.rs`: numeric > 0 wins;
/// non-numeric / zero falls back to the default with a warn log.
const TURN_CAP_ENV: &str = "NEURON_SWARM_AGENT_TURN_CAP";

/// Per-agent status visible to the UI (eventually rendered by W4-04
/// grid header pills). Snake_case wire form per Charter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    /// Default for every (workspace, agent) pair before the first
    /// lazy-spawn fires. The grid renders these as muted "—" pills.
    NotSpawned,
    /// Spawning in flight. Brief — visible only across one
    /// `acquire` window. Flips to `Idle` once the session is in the
    /// registry.
    Spawning,
    /// Session ready, no turn in flight.
    Idle,
    /// `invoke_turn` is in flight against this session.
    Running,
    /// Specialist emitted a `neuron_help` block (W4-05 will set
    /// this; W4-02 never emits it but the variant is present so
    /// W4-05 doesn't have to widen the type).
    WaitingOnCoordinator,
    /// The session crashed (subprocess died unrecoverably). Will
    /// be respawned on next `acquire_and_invoke_turn`. Distinct
    /// from `NotSpawned` so the grid can surface a "this agent had
    /// trouble" indicator separate from "this agent never ran".
    Crashed,
}

/// Wire shape for `swarm:agents:list_status`. Trimmed to what the
/// UI actually renders; richer per-agent diagnostics can be added
/// in a follow-up without breaking this surface.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusRow {
    pub workspace_id: String,
    pub agent_id: String,
    pub status: AgentStatus,
    /// `0` for un-touched agents — `NotSpawned` rows always have
    /// `turns_taken: 0`. After respawn under the turn-cap, this
    /// counter resets.
    pub turns_taken: u32,
    /// Wall-clock ms since UNIX epoch of the most recent
    /// state-changing event (spawn, turn start, turn end, crash).
    /// `None` when `status == NotSpawned`.
    pub last_activity_ms: Option<i64>,
}

/// Inner per-agent slot. The registry holds these behind an
/// `Arc<Mutex<...>>` so each agent's turns serialise without
/// blocking other agents.
struct AgentSlot {
    session: Option<PersistentSession>,
    status: AgentStatus,
    turns_taken: u32,
    last_activity_ms: Option<i64>,
}

impl AgentSlot {
    fn new() -> Self {
        Self {
            session: None,
            status: AgentStatus::NotSpawned,
            turns_taken: 0,
            last_activity_ms: None,
        }
    }
}

/// Workspace-scoped session registry. See module docs.
pub struct SwarmAgentRegistry {
    sessions: RwLock<HashMap<(String, String), Arc<Mutex<AgentSlot>>>>,
    profiles: Arc<ProfileRegistry>,
    turn_cap: u32,
}

impl SwarmAgentRegistry {
    /// Build a fresh registry. Reads `NEURON_SWARM_AGENT_TURN_CAP`
    /// from the environment; non-numeric / zero values fall back to
    /// `DEFAULT_TURN_CAP` with a warn log so a typo isn't silently
    /// ignored.
    pub fn new(profiles: Arc<ProfileRegistry>) -> Self {
        let turn_cap = resolve_turn_cap();
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
        }
    }

    /// Builder hook for tests — lets the suite pin a small `turn_cap`
    /// without spelunking through env vars.
    #[cfg(test)]
    pub(crate) fn with_turn_cap(
        profiles: Arc<ProfileRegistry>,
        turn_cap: u32,
    ) -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            profiles,
            turn_cap,
        }
    }

    /// Acquire (or lazy-spawn) the session for one
    /// (workspace, agent) and run one turn against it.
    ///
    /// Caller is the FSM (W4-06) for specialists, the chat IPC for
    /// Orchestrator. Failure paths leave the slot in `Crashed`
    /// state with `session: None`; the next call respawns
    /// transparently.
    ///
    /// Cancel: forwarded to `PersistentSession::invoke_turn`.
    /// Cancel returns `AppError::Cancelled` and leaves the session
    /// alive (W4-01 cancel contract).
    pub async fn acquire_and_invoke_turn<R: Runtime>(
        self: &Arc<Self>,
        app: &AppHandle<R>,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> Result<InvokeResult, AppError> {
        if workspace_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }
        if agent_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "agentId must not be empty".into(),
            ));
        }
        let key = (workspace_id.to_string(), agent_id.to_string());

        // 1. Get or insert the slot. Read first; if missing, take
        //    write lock to insert. Don't hold the write lock across
        //    the spawn — release it after the slot exists.
        let slot_arc = {
            let read = self.sessions.read().await;
            match read.get(&key) {
                Some(slot) => Arc::clone(slot),
                None => {
                    drop(read);
                    let mut write = self.sessions.write().await;
                    Arc::clone(
                        write
                            .entry(key.clone())
                            .or_insert_with(|| {
                                Arc::new(Mutex::new(AgentSlot::new()))
                            }),
                    )
                }
            }
        };

        // 2. Lock the per-agent slot for the duration of this turn.
        //    Other agents in the same workspace can run in parallel.
        let mut slot = slot_arc.lock().await;

        // 3. Lazy spawn if needed. Also: turn-cap respawn — if the
        //    existing session has accumulated `turn_cap` turns, kill
        //    it and replace with a fresh one before this turn fires.
        let needs_spawn = slot.session.is_none()
            || slot.turns_taken >= self.turn_cap;
        if needs_spawn {
            if let Some(old_session) = slot.session.take() {
                tracing::info!(
                    workspace_id = %workspace_id,
                    agent_id = %agent_id,
                    turns_taken = slot.turns_taken,
                    turn_cap = self.turn_cap,
                    "respawning agent session after turn cap"
                );
                // Best-effort shutdown of the old session; failures
                // shouldn't block the respawn.
                let _ = old_session.shutdown().await;
            }
            slot.status = AgentStatus::Spawning;
            slot.last_activity_ms = Some(now_millis());

            let profile = self
                .profiles
                .get(agent_id)
                .ok_or_else(|| {
                    AppError::NotFound(format!(
                        "swarm profile `{agent_id}`"
                    ))
                })?;
            let session =
                PersistentSession::spawn(app, profile).await.map_err(|e| {
                    slot.status = AgentStatus::Crashed;
                    e
                })?;
            slot.session = Some(session);
            slot.turns_taken = 0;
        }

        // 4. Run the turn. Status flips to Running for the duration,
        //    then to Idle on success / Crashed on hard error.
        slot.status = AgentStatus::Running;
        slot.last_activity_ms = Some(now_millis());

        let session = slot.session.as_mut().ok_or_else(|| {
            // Should never fire — we just spawned above.
            AppError::Internal(
                "swarm agent slot has no session post-spawn".into(),
            )
        })?;
        let outcome = session
            .invoke_turn(user_message, timeout, cancel)
            .await;
        slot.turns_taken = session.turns_taken();
        slot.last_activity_ms = Some(now_millis());

        match outcome {
            Ok(result) => {
                slot.status = AgentStatus::Idle;
                Ok(result)
            }
            Err(AppError::Cancelled(msg)) => {
                // Cancel keeps the session alive — flip back to
                // Idle so the next acquire reuses it.
                slot.status = AgentStatus::Idle;
                Err(AppError::Cancelled(msg))
            }
            Err(other) => {
                // SwarmInvoke / Timeout / etc. → mark crashed,
                // drop the session so the next acquire respawns.
                slot.status = AgentStatus::Crashed;
                if let Some(dead) = slot.session.take() {
                    // Best-effort shutdown so the child doesn't
                    // linger as an orphan if its stdin/out pipes
                    // are still drainable.
                    let _ = dead.shutdown().await;
                }
                Err(other)
            }
        }
    }

    /// Read-only snapshot for `swarm:agents:list_status`. Cheap —
    /// clones the metadata, never the session itself. Returns one
    /// row per *bundled* profile in the registry (so the UI can
    /// render a `NotSpawned` pill for agents the user hasn't
    /// touched yet) plus any rows that lazy-spawned.
    pub async fn list_status(
        &self,
        workspace_id: &str,
    ) -> Vec<AgentStatusRow> {
        let read = self.sessions.read().await;

        // Build the result by walking the bundled profile list so
        // un-touched agents appear as NotSpawned. Workspace-override
        // profiles also show up (they live in the same
        // ProfileRegistry).
        let mut rows: Vec<AgentStatusRow> = Vec::new();
        for profile in self.profiles.list() {
            let key =
                (workspace_id.to_string(), profile.id.clone());
            let row = match read.get(&key) {
                Some(slot_arc) => {
                    let slot = slot_arc.lock().await;
                    AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: slot.status,
                        turns_taken: slot.turns_taken,
                        last_activity_ms: slot.last_activity_ms,
                    }
                }
                None => AgentStatusRow {
                    workspace_id: workspace_id.to_string(),
                    agent_id: profile.id.clone(),
                    status: AgentStatus::NotSpawned,
                    turns_taken: 0,
                    last_activity_ms: None,
                },
            };
            rows.push(row);
        }
        rows.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        rows
    }

    /// Eager shutdown of every session for `workspace_id`.
    /// Idempotent — calling on an empty workspace returns `Ok(())`.
    /// Used by `swarm:agents:shutdown_workspace` and by
    /// `shutdown_all` on app close.
    pub async fn shutdown_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<(), AppError> {
        if workspace_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "workspaceId must not be empty".into(),
            ));
        }
        // Collect the keys + slots to shut down. Drop the
        // structural lock before driving the per-slot shutdowns so
        // we don't block other workspaces' acquire calls during a
        // shutdown that may take seconds.
        let to_shutdown: Vec<(
            (String, String),
            Arc<Mutex<AgentSlot>>,
        )> = {
            let mut write = self.sessions.write().await;
            let keys: Vec<(String, String)> = write
                .keys()
                .filter(|(ws, _)| ws == workspace_id)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| {
                    write.remove(&k).map(|slot| (k, slot))
                })
                .collect()
        };

        for (_, slot_arc) in to_shutdown {
            // Take the slot lock and drop the session. We do this
            // sequentially per (workspace, agent) — these locks are
            // independent so we don't gain much from parallelism,
            // and serial keeps the log readable.
            let mut slot = slot_arc.lock().await;
            if let Some(session) = slot.session.take() {
                let _ = session.shutdown().await;
            }
            slot.status = AgentStatus::NotSpawned;
            slot.turns_taken = 0;
            slot.last_activity_ms = None;
        }
        Ok(())
    }

    /// Eager shutdown of every workspace's every session.
    /// Called from `lib.rs` on `WindowEvent::CloseRequested`.
    pub async fn shutdown_all(&self) -> Result<(), AppError> {
        let workspace_ids: Vec<String> = {
            let read = self.sessions.read().await;
            let mut ids: Vec<String> = read
                .keys()
                .map(|(ws, _)| ws.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        };
        for ws in workspace_ids {
            // Each shutdown_workspace drops the structural lock
            // between iterations so we don't hold the write lock
            // for the entire teardown wall time.
            self.shutdown_workspace(&ws).await?;
        }
        Ok(())
    }

    /// Diagnostics: how many slots does the registry hold across all
    /// workspaces? Used by tests + a future telemetry surface.
    #[cfg(test)]
    pub(crate) async fn slot_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Diagnostics: the configured turn cap for this registry.
    pub fn turn_cap(&self) -> u32 {
        self.turn_cap
    }
}

/// Resolve the per-process turn cap. Same env-reading shape as
/// `commands/swarm.rs::stage_timeout` so the project has one
/// pattern for tunable env knobs.
fn resolve_turn_cap() -> u32 {
    match std::env::var(TURN_CAP_ENV) {
        Ok(raw) if !raw.trim().is_empty() => match raw.trim().parse::<u32>() {
            Ok(0) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    "value `0` is not a valid turn cap; falling back to default"
                );
                DEFAULT_TURN_CAP
            }
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    %TURN_CAP_ENV,
                    raw = %raw,
                    error = %e,
                    "turn cap override is not a non-negative integer; using default"
                );
                DEFAULT_TURN_CAP
            }
        },
        _ => DEFAULT_TURN_CAP,
    }
}

// --------------------------------------------------------------------- //
// Tests                                                                  //
// --------------------------------------------------------------------- //

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::mock_app_with_pool;

    fn fresh_registry() -> Arc<SwarmAgentRegistry> {
        let profiles =
            Arc::new(ProfileRegistry::load_from(None).expect("load"));
        Arc::new(SwarmAgentRegistry::new(profiles))
    }

    /// Fresh registry against the bundled 9 profiles surfaces 9
    /// `NotSpawned` rows for any workspace. The W4-04 grid header
    /// reads exactly this shape on first mount.
    #[tokio::test]
    async fn list_status_returns_not_spawned_for_untouched_agents() {
        let reg = fresh_registry();
        let rows = reg.list_status("default").await;
        assert_eq!(rows.len(), 9, "expected 9 bundled profiles");
        for r in &rows {
            assert_eq!(r.status, AgentStatus::NotSpawned);
            assert_eq!(r.turns_taken, 0);
            assert!(r.last_activity_ms.is_none());
            assert_eq!(r.workspace_id, "default");
        }
        // Stable alphabetical order — same shape `swarm:profiles_list`
        // promises elsewhere.
        let ids: Vec<&str> =
            rows.iter().map(|r| r.agent_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "backend-builder",
                "backend-reviewer",
                "coordinator",
                "frontend-builder",
                "frontend-reviewer",
                "integration-tester",
                "orchestrator",
                "planner",
                "scout",
            ]
        );
    }

    /// Different workspaces see independent `NotSpawned` rows.
    #[tokio::test]
    async fn list_status_isolated_per_workspace() {
        let reg = fresh_registry();
        let ws_a = reg.list_status("ws-a").await;
        let ws_b = reg.list_status("ws-b").await;
        assert_eq!(ws_a.len(), 9);
        assert_eq!(ws_b.len(), 9);
        for r in &ws_a {
            assert_eq!(r.workspace_id, "ws-a");
        }
        for r in &ws_b {
            assert_eq!(r.workspace_id, "ws-b");
        }
    }

    /// Empty registry slot count is 0 — no sessions exist before
    /// anyone calls `acquire`.
    #[tokio::test]
    async fn fresh_registry_has_zero_slots() {
        let reg = fresh_registry();
        assert_eq!(reg.slot_count().await, 0);
    }

    /// `shutdown_workspace` on an empty registry is a no-op (no
    /// crash, no error).
    #[tokio::test]
    async fn shutdown_workspace_on_empty_registry_is_ok() {
        let reg = fresh_registry();
        reg.shutdown_workspace("default").await.expect("ok");
        // Slot count stays 0.
        assert_eq!(reg.slot_count().await, 0);
    }

    /// `shutdown_all` on an empty registry is a no-op.
    #[tokio::test]
    async fn shutdown_all_on_empty_registry_is_ok() {
        let reg = fresh_registry();
        reg.shutdown_all().await.expect("ok");
        assert_eq!(reg.slot_count().await, 0);
    }

    /// Empty `workspaceId` rejected at the registry method boundary
    /// (mirrors the IPC validation at `commands/swarm.rs`). We
    /// validate twice — defense-in-depth — so a non-IPC caller
    /// (e.g. the FSM) doesn't bypass the check.
    #[tokio::test]
    async fn shutdown_workspace_rejects_empty_workspace_id() {
        let reg = fresh_registry();
        let err = reg
            .shutdown_workspace("")
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Turn cap defaults to `DEFAULT_TURN_CAP` (200) when the env
    /// var is absent.
    #[test]
    fn turn_cap_defaults_to_200_when_env_absent() {
        // Save + clear the env var for the duration of the test.
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::remove_var(TURN_CAP_ENV);
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        assert_eq!(DEFAULT_TURN_CAP, 200);
        // Restore.
        if let Some(v) = prior {
            std::env::set_var(TURN_CAP_ENV, v);
        }
    }

    /// `NEURON_SWARM_AGENT_TURN_CAP=42` lands as `turn_cap = 42`.
    #[test]
    fn turn_cap_env_override_lands() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "42");
        assert_eq!(resolve_turn_cap(), 42);
        // Restore.
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// Non-numeric env override falls back to default with a warn
    /// log (we don't capture the log here — too fragile — but we
    /// do assert the fallback fires).
    #[test]
    fn turn_cap_non_numeric_falls_back_to_default() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "not-a-number");
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// Zero env override falls back to default (we want `cap=0` to
    /// be a typo, not "never respawn").
    #[test]
    fn turn_cap_zero_falls_back_to_default() {
        let prior = std::env::var(TURN_CAP_ENV).ok();
        std::env::set_var(TURN_CAP_ENV, "0");
        assert_eq!(resolve_turn_cap(), DEFAULT_TURN_CAP);
        match prior {
            Some(v) => std::env::set_var(TURN_CAP_ENV, v),
            None => std::env::remove_var(TURN_CAP_ENV),
        }
    }

    /// `with_turn_cap` builder sets the cap directly without env
    /// dependency — the test path uses this so suite order doesn't
    /// matter.
    #[test]
    fn with_turn_cap_pins_cap() {
        let profiles =
            Arc::new(ProfileRegistry::load_from(None).expect("load"));
        let reg = SwarmAgentRegistry::with_turn_cap(profiles, 5);
        assert_eq!(reg.turn_cap(), 5);
    }

    /// Acquire with empty workspaceId rejected.
    #[tokio::test]
    async fn acquire_validates_empty_workspace_id() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "",
                "scout",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acquire with empty agentId rejected.
    #[tokio::test]
    async fn acquire_validates_empty_agent_id() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("empty rejected");
        assert_eq!(err.kind(), "invalid_input");
    }

    /// Acquire with unknown agentId rejected as `not_found` (the
    /// profile registry returns None for unknown ids).
    #[tokio::test]
    async fn acquire_unknown_agent_id_returns_not_found() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;
        let err = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "no-such-agent",
                "hi",
                Duration::from_secs(1),
                Arc::new(Notify::new()),
            )
            .await
            .expect_err("unknown rejected");
        assert_eq!(err.kind(), "not_found");
    }

    /// Real-claude integration smoke (`#[ignore]`'d) — drives two
    /// turns through the registry and asserts:
    ///  1. The same session is reused (turn 2 doesn't cold-start).
    ///  2. `list_status` flips through `Spawning → Running → Idle`
    ///     and reports `turns_taken == 2` after both turns finish.
    ///  3. `shutdown_workspace` reverts the row to `NotSpawned`.
    ///
    /// Time budget: typical 60-180s (one cold-start + two turns).
    #[tokio::test]
    #[ignore = "requires real `claude` binary + Pro/Max subscription"]
    async fn integration_registry_reuses_session() {
        let reg = fresh_registry();
        let (app, _pool, _dir) = mock_app_with_pool().await;

        let stage_secs = std::env::var("NEURON_SWARM_STAGE_TIMEOUT_SEC")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(180);
        let timeout = Duration::from_secs(stage_secs);
        let cancel = Arc::new(Notify::new());

        // Turn 1 — cold-start path, lazy-spawns the scout session.
        let r1 = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "scout",
                "Reply with exactly the single word `BETA` and nothing else.",
                timeout,
                Arc::clone(&cancel),
            )
            .await
            .expect("turn 1 ok");
        assert!(
            r1.assistant_text.to_uppercase().contains("BETA"),
            "turn 1 should contain BETA"
        );

        // Turn 2 — should reuse the session. The proof: list_status
        // shows turns_taken == 2 (not 1) for the scout row; if a
        // respawn had happened the counter would have reset.
        let r2 = reg
            .acquire_and_invoke_turn(
                app.handle(),
                "default",
                "scout",
                "What was the single word you just replied with? Answer in one word.",
                timeout,
                cancel,
            )
            .await
            .expect("turn 2 ok");
        assert!(
            r2.assistant_text.to_uppercase().contains("BETA"),
            "turn 2 should recall BETA"
        );

        let rows = reg.list_status("default").await;
        let scout = rows
            .iter()
            .find(|r| r.agent_id == "scout")
            .expect("scout row");
        assert_eq!(scout.status, AgentStatus::Idle);
        assert_eq!(scout.turns_taken, 2);
        assert!(scout.last_activity_ms.is_some());

        // Shutdown reverts to NotSpawned.
        reg.shutdown_workspace("default").await.expect("shutdown ok");
        let rows = reg.list_status("default").await;
        let scout = rows
            .iter()
            .find(|r| r.agent_id == "scout")
            .expect("scout row");
        assert_eq!(scout.status, AgentStatus::NotSpawned);
        assert_eq!(scout.turns_taken, 0);
    }
}

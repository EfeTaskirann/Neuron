//! `list_status` — the read-only snapshot surfaced by
//! `swarm:agents:list_status`. Split out of [`super`] so the read
//! path sits apart from the mutating turn/lifecycle code.

use crate::time::now_millis;

use super::super::status::{AgentStatus, AgentStatusRow};
use super::SwarmAgentRegistry;

impl SwarmAgentRegistry {
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
                Some(slot_arc) => match slot_arc.try_lock() {
                    Ok(slot) => AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: slot.status,
                        turns_taken: slot.turns_taken,
                        last_activity_ms: slot.last_activity_ms,
                    },
                    // CONC-02: the slot is locked → an `invoke_turn` is in
                    // flight. Do NOT block on it here: this runs while
                    // holding the `sessions` RwLock read, and blocking for
                    // a full turn would stall a concurrent new-agent spawn
                    // waiting on the write lock. Report `Running` (accurate
                    // by construction); the next poll reads exact counters
                    // once the turn releases the slot.
                    Err(_) => AgentStatusRow {
                        workspace_id: workspace_id.to_string(),
                        agent_id: profile.id.clone(),
                        status: AgentStatus::Running,
                        turns_taken: 0,
                        last_activity_ms: Some(now_millis()),
                    },
                },
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
}

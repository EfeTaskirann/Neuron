//! The user's cost controls for a session (Settings → Models): which team
//! runs, each agent's model, and how many messages the agents may send
//! one another before the swarm holds.
//!
//! Kept in Neuron's settings table as one JSON value and read when a
//! session starts; a change applies to the next session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::commands::settings;
use crate::db::DbPool;
use crate::error::AppError;
use crate::swarm_term::hierarchy::AGENT_IDS;

/// Settings key of the [`SwarmTermTeam`] JSON.
const SETTINGS_KEY: &str = "swarm_term.team";

/// The smallest team that still runs the whole task cycle — the
/// coordinator owns the lifecycle, so it cannot be left out. The builder
/// and the reviewer take on every kind of work (see the persona footer).
pub const LITE_TEAM: &[&str] = &["orchestrator", "coordinator", "backend-builder", "backend-reviewer"];

/// The models a pane may run: claude's aliases for the newest model of
/// each family. No alias: claude's own default.
pub const MODEL_ALIASES: &[&str] = &["opus", "sonnet", "haiku"];

/// Agent-to-agent messages a session delivers before it holds, unless
/// the user set another budget.
pub const DEFAULT_MESSAGE_BUDGET: u32 = 300;

/// A budget above this is no budget at all; refused as a typo.
const MAX_MESSAGE_BUDGET: u32 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SwarmTermTeam {
    /// Run the Lite team ([`LITE_TEAM`]) instead of all nine agents.
    pub lite: bool,
    /// Agent id → model alias ([`MODEL_ALIASES`]). An agent not named
    /// runs claude's default model.
    pub models: BTreeMap<String, String>,
    /// Agent-to-agent messages a session may deliver before it holds;
    /// `0`: no limit.
    pub message_budget: u32,
}

impl Default for SwarmTermTeam {
    fn default() -> Self {
        Self { lite: false, models: BTreeMap::new(), message_budget: DEFAULT_MESSAGE_BUDGET }
    }
}

impl SwarmTermTeam {
    /// The agents a session runs with these settings, in spawn order.
    pub fn agents(&self) -> Vec<&'static str> {
        AGENT_IDS
            .iter()
            .copied()
            .filter(|agent| !self.lite || LITE_TEAM.contains(agent))
            .collect()
    }

    /// The model alias `agent` runs, if one is set.
    pub fn model_of(&self, agent: &str) -> Option<&str> {
        self.models.get(agent).map(String::as_str)
    }

    fn check(&self) -> Result<(), AppError> {
        for (agent, model) in &self.models {
            if !AGENT_IDS.contains(&agent.as_str()) {
                return Err(AppError::InvalidInput(format!("unknown agent `{agent}`")));
            }
            if !MODEL_ALIASES.contains(&model.as_str()) {
                return Err(AppError::InvalidInput(format!(
                    "unknown model `{model}` (one of: {})",
                    MODEL_ALIASES.join(", ")
                )));
            }
        }
        if self.message_budget > MAX_MESSAGE_BUDGET {
            return Err(AppError::InvalidInput(format!(
                "message budget {} is above {MAX_MESSAGE_BUDGET}",
                self.message_budget
            )));
        }
        Ok(())
    }
}

/// The saved settings; unset or unreadable ones read as the defaults.
pub async fn load(pool: &DbPool) -> Result<SwarmTermTeam, AppError> {
    Ok(settings::read(pool, SETTINGS_KEY)
        .await?
        .and_then(|raw| serde_json::from_str::<SwarmTermTeam>(&raw).ok())
        .filter(|team| team.check().is_ok())
        .unwrap_or_default())
}

/// Save the settings, after checking every agent and model name.
pub async fn save(pool: &DbPool, team: &SwarmTermTeam) -> Result<(), AppError> {
    team.check()?;
    let json = serde_json::to_string(team)
        .map_err(|e| AppError::Internal(format!("team settings: {e}")))?;
    settings::write(pool, SETTINGS_KEY, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lite_team_keeps_the_cycle_and_spawn_order() {
        let full = SwarmTermTeam::default();
        assert_eq!(full.agents(), AGENT_IDS);
        let lite = SwarmTermTeam { lite: true, ..SwarmTermTeam::default() };
        assert_eq!(lite.agents(), LITE_TEAM);
        // Every lite agent is a real one, and the lifecycle owner is there.
        assert!(LITE_TEAM.iter().all(|a| AGENT_IDS.contains(a)));
        assert!(LITE_TEAM.contains(&"coordinator"));
        assert_eq!(
            crate::swarm_term::hierarchy::reviewer_for_builder("backend-builder"),
            Some("backend-reviewer")
        );
    }

    #[tokio::test]
    async fn settings_are_checked_saved_and_read_back() {
        let (pool, _db) = crate::test_support::fresh_pool().await;
        assert_eq!(load(&pool).await.unwrap(), SwarmTermTeam::default());
        let team = SwarmTermTeam {
            lite: true,
            models: BTreeMap::from([
                ("orchestrator".to_string(), "opus".to_string()),
                ("backend-builder".to_string(), "sonnet".to_string()),
            ]),
            message_budget: 50,
        };
        save(&pool, &team).await.unwrap();
        let back = load(&pool).await.unwrap();
        assert_eq!(back, team);
        assert_eq!(back.model_of("orchestrator"), Some("opus"));
        assert_eq!(back.model_of("coordinator"), None);

        for bad in [
            SwarmTermTeam { models: BTreeMap::from([("nobody".into(), "opus".into())]), ..team.clone() },
            SwarmTermTeam { models: BTreeMap::from([("scout".into(), "gpt-5".into())]), ..team.clone() },
            SwarmTermTeam { models: BTreeMap::from([("scout".into(), "opus --dangerously".into())]), ..team.clone() },
            SwarmTermTeam { message_budget: MAX_MESSAGE_BUDGET + 1, ..team.clone() },
        ] {
            assert!(save(&pool, &bad).await.is_err(), "{bad:?}");
        }
        // A value someone wrote by hand that no longer checks out reads
        // as the defaults instead of failing every start.
        settings::write(&pool, SETTINGS_KEY, r#"{"lite":true,"models":{"scout":"gpt-5"},"messageBudget":5}"#)
            .await
            .unwrap();
        assert_eq!(load(&pool).await.unwrap(), SwarmTermTeam::default());
    }
}

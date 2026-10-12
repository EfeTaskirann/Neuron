//! Persisted-history read accessors for [`JobRegistry`]. The IPC
//! layer (`swarm:list_jobs` / `swarm:get_job`) calls through here so
//! the SQL helpers in [`crate::swarm::coordinator::store`] stay
//! `pub(super)`. Without a pool these return the empty/`None`
//! signal — the in-memory cache is the FSM's working state, not a
//! history surface.

use crate::error::AppError;
use crate::swarm::coordinator::store;

use super::super::model::{JobDetail, JobSummary};
use super::JobRegistry;

impl JobRegistry {
    /// List recent jobs from the persisted history. The IPC layer
    /// (`swarm:list_jobs`) calls through here so the SQL helpers
    /// stay `pub(super)`. Without a pool, returns an empty Vec —
    /// the in-memory cache is the FSM's working state, not a
    /// history surface.
    pub async fn list_jobs(
        &self,
        workspace_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<JobSummary>, AppError> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };
        store::list_jobs(pool, workspace_id, limit).await
    }

    /// Fetch one job detail (job + stages) from the persisted
    /// history. Used by `swarm:get_job`. `Ok(None)` is the
    /// canonical "unknown id" signal — the IPC layer maps to
    /// `AppError::NotFound`.
    pub async fn get_job_detail(
        &self,
        job_id: &str,
    ) -> Result<Option<JobDetail>, AppError> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };
        store::get_job_detail(pool, job_id).await
    }
}

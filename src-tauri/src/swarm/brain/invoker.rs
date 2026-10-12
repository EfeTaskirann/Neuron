//! `CoordinatorInvoker` — test-injection seam over the registry.
//!
//! Split out of the monolithic `brain.rs` (WP-W5-03). Production
//! impl forwards to `SwarmAgentRegistry::acquire_and_invoke_turn`
//! with `agent_id = "coordinator"`; mock impls (in `tests.rs`)
//! return canned action sequences.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::transport::InvokeResult;

/// Test-injection seam over `SwarmAgentRegistry::acquire_and_invoke_turn`
/// for the `coordinator` agent specifically. Production impl
/// delegates straight through; mock impls return canned action
/// sequences that drive the brain through scripted scenarios.
///
/// Same shape as [`crate::swarm::AgentInvoker`]: one method,
/// returning `impl Future`, no `async-trait` dep (Charter §"no new
/// deps").
pub trait CoordinatorInvoker: Send + Sync + 'static {
    /// Invoke one turn against the workspace's `coordinator`
    /// session. The brain calls this at the start of every loop
    /// iteration; the returned `assistant_text` is fed into
    /// [`super::parse_brain_action`].
    fn invoke_coordinator_turn(
        &self,
        workspace_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send;
}

/// Production impl: forwards to
/// `SwarmAgentRegistry::acquire_and_invoke_turn` with
/// `agent_id = "coordinator"`. Mirrors the W5-02
/// `SwarmAgentRegistryInvoker` shape so the construction sites in
/// `commands::swarm` can compose both with the same handle.
pub struct SwarmRegistryCoordinatorInvoker<R: Runtime> {
    app: AppHandle<R>,
    registry: Arc<SwarmAgentRegistry>,
}

impl<R: Runtime> SwarmRegistryCoordinatorInvoker<R> {
    pub fn new(
        app: AppHandle<R>,
        registry: Arc<SwarmAgentRegistry>,
    ) -> Self {
        Self { app, registry }
    }
}

impl<R: Runtime> CoordinatorInvoker for SwarmRegistryCoordinatorInvoker<R> {
    fn invoke_coordinator_turn(
        &self,
        workspace_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send {
        let registry = Arc::clone(&self.registry);
        let app = self.app.clone();
        let workspace_id = workspace_id.to_string();
        let user_message = user_message.to_string();
        async move {
            registry
                .acquire_and_invoke_turn(
                    &app,
                    &workspace_id,
                    "coordinator",
                    &user_message,
                    timeout,
                    cancel,
                )
                .await
        }
    }
}

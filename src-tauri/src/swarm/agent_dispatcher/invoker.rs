//! `AgentInvoker` — the test-injection seam over the registry's
//! invoke surface, plus the production impl that delegates to
//! `SwarmAgentRegistry::acquire_and_invoke_turn`.

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Runtime};
use tokio::sync::Notify;

use crate::error::AppError;
use crate::swarm::agent_registry::SwarmAgentRegistry;
use crate::swarm::transport::InvokeResult;

/// Test-injection seam over `SwarmAgentRegistry::acquire_and_invoke_turn`.
/// One method; production impl delegates straight through, mock
/// impls return canned `InvokeResult`s without spawning `claude`.
///
/// Same pattern as `swarm::transport::Transport`: returns
/// `impl Future` (stable since 1.75) instead of `async fn` so we
/// don't need `async-trait` (Charter §"no new deps").
///
/// `Send + Sync + 'static` so the dispatcher can spawn a tokio task
/// holding an `Arc<I>` without lifetime juggling.
pub trait AgentInvoker: Send + Sync + 'static {
    /// Invoke one turn against the named (workspace, agent). Cancel
    /// is forwarded to the underlying session's
    /// `PersistentSession::invoke_turn`.
    fn invoke_turn(
        &self,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send;
}

/// Production impl: forwards to
/// `SwarmAgentRegistry::acquire_and_invoke_turn` (no help loop —
/// help loop is W5-03 scope).
pub struct SwarmAgentRegistryInvoker<R: Runtime> {
    app: AppHandle<R>,
    registry: Arc<SwarmAgentRegistry>,
}

impl<R: Runtime> SwarmAgentRegistryInvoker<R> {
    pub fn new(
        app: AppHandle<R>,
        registry: Arc<SwarmAgentRegistry>,
    ) -> Self {
        Self { app, registry }
    }
}

impl<R: Runtime> AgentInvoker for SwarmAgentRegistryInvoker<R> {
    fn invoke_turn(
        &self,
        workspace_id: &str,
        agent_id: &str,
        user_message: &str,
        timeout: Duration,
        cancel: Arc<Notify>,
    ) -> impl std::future::Future<Output = Result<InvokeResult, AppError>>
           + Send {
        let registry = Arc::clone(&self.registry);
        let app = self.app.clone();
        let workspace_id = workspace_id.to_string();
        let agent_id = agent_id.to_string();
        let user_message = user_message.to_string();
        async move {
            registry
                .acquire_and_invoke_turn(
                    &app,
                    &workspace_id,
                    &agent_id,
                    &user_message,
                    timeout,
                    cancel,
                )
                .await
        }
    }
}

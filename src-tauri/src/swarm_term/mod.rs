//! Terminal-Hierarchy Swarm — parallel feature to the W5 mailbox-bus
//! swarm. Spawns 9 visible PTY panes (via `sidecar::terminal::TerminalRegistry`),
//! injects each persona as the first stdin write, and intercepts
//! `>> @agent:` markers in pane output to paste-route messages
//! between terminals with a `— from @sender` signature, gated by
//! a hierarchy graph in `hierarchy.rs`.

pub mod hierarchy;
pub mod marker;
pub mod router;
pub mod session;

pub use session::{TerminalSwarmRegistry, TerminalSwarmSessionHandle};

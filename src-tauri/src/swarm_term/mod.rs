//! Terminal-Hierarchy Swarm — 9 visible PTY panes (via
//! `sidecar::terminal::TerminalRegistry`), each running a `claude` REPL
//! on the user's own Claude configuration (see [`pane_config`]),
//! communicating through a file-system based IPC bridge.
//!
//! ## How inter-agent messaging works
//!
//! Each pane is started with `NEURON_BRIDGE`, `NEURON_AGENT_ID`, and
//! `NEURON_OUTBOX` environment variables that pin the per-session
//! `.bridgespace/<session>/` directory. When agent A wants to message
//! agent B, A uses its `Write` tool to atomically create
//! `<NEURON_BRIDGE>/outbox/<A>/<id>.json` with a small JSON envelope
//! (`{to, body, task_id?, from?}` — the sender is the outbox directory).
//! A background poll loop in [`bridge::watcher::watcher_loop`] reads every
//! outbox once every 250 ms, validates the envelope, applies the
//! hierarchy gate from [`hierarchy`], delivers the body to the target
//! pane's PTY via bracketed paste, and migrates the file to
//! `processed/<B>/`. Denied or malformed files go to `rejected/<B>/` with
//! a `.reason` sidecar. The hierarchy keeps the swarm orderly; it is not
//! a security boundary (see `bridge` § Trust model).
//!
//! ## Why files instead of `>> @target:` PTY markers
//!
//! The earlier (pre-2026-05-15) design parsed marker lines out of every
//! pane's PTY output. That required tolerating claude's streaming
//! repaint loop, mid-stream cursor positioning, markdown decorators,
//! Unicode glyph variants, and bracketed-paste round-trips — over
//! 1500 lines of PTY-parsing fragility that the file-based design
//! eliminates wholesale. Each message is now one atomic JSON file: no
//! stream debounce, no near-miss diagnostic, no body assembler, no
//! repaint dedupe.

pub mod bridge;
pub(crate) mod heartbeat;
pub mod hierarchy;
pub mod lifecycle;
pub(crate) mod pane_config;
pub(crate) mod permissions;
pub mod persona;
pub(crate) mod project_guard;
pub mod session;
pub(crate) mod trust;

pub use permissions::SwarmTermPermissionMode;
pub use session::{TerminalSwarmRegistry, TerminalSwarmSessionHandle};

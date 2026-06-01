//! Terminal-Hierarchy Swarm — 9 visible PTY panes (via
//! `sidecar::terminal::TerminalRegistry`), each running an isolated
//! `claude` REPL, communicating through a file-system based IPC bridge.
//!
//! ## How inter-agent messaging works
//!
//! Each pane is started with `NEURON_BRIDGE`, `NEURON_AGENT_ID`, and
//! `NEURON_INBOX` environment variables that pin the per-session
//! `.bridgespace/<session>/` directory. When agent A wants to message
//! agent B, A uses its `Write` tool to atomically create
//! `<NEURON_BRIDGE>/inbox/<B>/<id>.json` with a small JSON envelope
//! (`{from, to, body, task_id?}`). A background poll loop in
//! [`bridge::watcher_loop`] reads every inbox directory once every
//! 250 ms, validates the envelope, applies the hierarchy gate from
//! [`hierarchy`], delivers the body to the target pane's PTY via
//! bracketed paste, and migrates the file to `processed/<B>/`. Denied
//! or malformed files go to `rejected/<B>/` with a `.reason` sidecar.
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
pub mod hierarchy;
pub mod home_isolation;
pub mod lifecycle;
pub mod persona;
pub mod session;

pub use session::{TerminalSwarmRegistry, TerminalSwarmSessionHandle};

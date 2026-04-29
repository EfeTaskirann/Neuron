//! Sidecar process supervisors.
//!
//! WP-W2-04 introduces the LangGraph Python sidecar. WP-W2-06 layered
//! the portable-pty terminal supervisor alongside it.
//!
//! Each module owns:
//!
//! - The child-process spawn / shutdown lifecycle.
//! - The framing / RPC convention for that sidecar.
//! - A typed handle (`SidecarHandle` for the Python sidecar,
//!   `TerminalRegistry` for PTYs) that Tauri commands inject via
//!   `State<...>` and use to dispatch work into the child(ren).

pub mod agent;
pub mod framing;
pub mod terminal;

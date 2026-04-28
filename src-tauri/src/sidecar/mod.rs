//! Sidecar process supervisors.
//!
//! WP-W2-04 introduces the LangGraph Python sidecar. Future packages
//! will add the portable-pty terminal sidecar (WP-W2-06) here as well.
//!
//! Each module owns:
//!
//! - The child-process spawn / shutdown lifecycle.
//! - The framing / RPC convention for that sidecar.
//! - A typed handle (`SidecarHandle`-ish) that Tauri commands inject
//!   via `State<...>` and use to dispatch work into the child.

pub mod agent;
pub mod framing;

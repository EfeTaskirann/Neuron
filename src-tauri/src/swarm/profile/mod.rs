//! `.md`-backed agent profile loader (WP-W3-11 §2).
//!
//! Profiles are markdown files with a YAML-ish frontmatter block bound
//! by `^---$` lines. The body (everything after the closing `---`) is
//! the persona prompt fed into `claude --append-system-prompt-file`.
//!
//! Two source roots feed the registry, in order (workspace wins on
//! `id` collision per WP §2):
//!
//! 1. `<app_data_dir>/agents/*.md` — user-edited workspace overrides.
//!    Optional; missing dir is not an error.
//! 2. Bundled defaults embedded via `include_dir!` from
//!    `src-tauri/src/swarm/agents/*.md` — three personas
//!    (`scout`, `planner`, `backend-builder`) ship with the binary.
//!
//! Frontmatter is hand-parsed (no `gray_matter` / `serde_yaml` dep —
//! see WP §"Sub-agent reminders"). The contract is intentionally
//! narrow: only the nine fields listed in `Profile` are read; extras
//! are tolerated but ignored so W3-12 can extend the schema without
//! breaking existing profiles.
//!
//! ## Module layout
//!
//! This file was split out of a single 1007-line `profile.rs` along
//! responsibility lines; the public surface is re-exported here so the
//! `swarm::profile::{…}` paths consumers use are unchanged:
//!
//! - [`types`]: `PermissionMode`, `Profile`, `ProfileSource` data shapes.
//! - [`parser`]: the hand-rolled frontmatter parser.
//! - [`registry`]: the `ProfileRegistry` loader (bundled + workspace).

mod parser;
mod registry;
mod types;

#[cfg(test)]
mod tests;

pub use registry::ProfileRegistry;
pub use types::{PermissionMode, Profile, ProfileSource};

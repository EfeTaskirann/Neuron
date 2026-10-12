//! WP-W3-06 — telemetry module tests.
//!
//! Split from the former single-file `tests.rs` (617L) into per-concern
//! submodules (DEEP) that mirror the production split
//! (`otlp` / `exporter`). The test bodies are moved verbatim — only the
//! module wrapping changed; every test reaches the package types through
//! the `crate::telemetry::{otlp, exporter}` re-exports, so the assertions
//! are the byte-for-byte pre-split versions.
//!
//! The pre-split file's helpers each had a single consumer group, so
//! they now live with that group rather than in shared `mod.rs`
//! scaffolding: `fixture_batch` / `opts_with_prompts` / the fixture
//! `include_str!` belong to [`envelope`], `seed_pending_spans` to
//! [`sweep`], and the `ResultExt` shim to [`endpoint`].
//!
//! Layout:
//! - [`envelope`] — OTLP shape: round-trip a known span batch through
//!   [`crate::telemetry::otlp::build_envelope`], deterministic ids,
//!   default-deny redaction, root-span / duration / truncation edges.
//! - [`migration`] — schema-level checks for the `0005` migration
//!   (column count grows by 2; partial index lands in `sqlite_master`).
//! - [`sweep`] — drive [`crate::telemetry::exporter::export_one_batch`]
//!   against a `mockito` stub HTTP server through the documented
//!   status-code branches (2xx / 4xx / 5xx / empty / full batch) plus
//!   the in-flight / unsampled SQL-filter guards.
//! - [`endpoint`] — security-review M2 / M3: `validate_endpoint`
//!   scheme/host rules + the large-body no-OOM sweep.

mod endpoint;
mod envelope;
mod migration;
mod sweep;

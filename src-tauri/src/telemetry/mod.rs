//! WP-W3-06 — OTel span export.
//!
//! `runs_spans` is the source-of-truth for everything the run
//! inspector renders, but we also want the data to flow into a
//! standards-compliant OTLP collector so external dashboards (Grafana
//! Tempo, Jaeger, Honeycomb, etc.) can consume Neuron traces side-by-
//! side with the rest of an organisation's observability fleet.
//!
//! Layout
//! ------
//!
//! - [`sampling`] — per-span sampling decision driven by
//!   `NEURON_OTEL_SAMPLING_RATIO` (env var, 0.0..=1.0). Applied at
//!   `agent::insert_span` time so the export sweep does not have to
//!   re-decide on every loop iteration.
//! - [`otlp`] — manual OTLP/HTTP/JSON v1.3 envelope serializer. No
//!   SDK dependency: the wire shape is small enough that hand-rolling
//!   it stays cheaper than pulling `opentelemetry`'s tower of crates,
//!   and it keeps the dep tree under the Charter's tech-stack table.
//! - [`exporter`] — periodic sweep task that reads pending rows out
//!   of `runs_spans`, builds an envelope, POSTs to the configured
//!   endpoint, and updates `exported_at` on success.
//!
//! Wiring
//! ------
//!
//! `lib.rs::run().setup` reads `NEURON_OTEL_ENDPOINT` and, iff non-
//! empty, spawns [`start_export_loop`] on the Tauri tokio runtime.
//! When the env var is unset the loop never starts and the rest of
//! the app behaves identically — span rows still accumulate in
//! SQLite, they're just never POSTed anywhere.

pub mod exporter;
pub mod otlp;
pub mod sampling;

#[cfg(test)]
mod tests;

pub use exporter::start_export_loop;

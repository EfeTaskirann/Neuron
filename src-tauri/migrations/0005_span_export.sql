-- 0005 — OTel span export bookkeeping (WP-W3-06).
--
-- Two new columns on `runs_spans`:
--
--   `exported_at` — unix seconds when the row was successfully POSTed
--   to the configured OTLP/HTTP collector. NULL = pending; positive
--   integer = exported; -1 = sentinel for permanent failure (the
--   collector returned 4xx, so retrying would just bounce again).
--
--   `sampled_in` — sampling decision made at insert time (per-span,
--   not per-run). 0 = sampled out, 1 = include in export. Default 1
--   keeps existing rows (and rows from any code path that does not
--   set the column) eligible for export — a conservative default
--   that errs on the side of completeness.
--
-- Partial index `idx_runs_spans_export_pending` powers the export
-- sweep's hot path: `WHERE exported_at IS NULL AND sampled_in = 1`.
-- The predicate naturally skips both the `-1` permanent-failure
-- sentinel and rows excluded by sampling, so the sweep's scan stays
-- proportional to actual pending work rather than total span count.
--
-- See `src-tauri/src/telemetry/exporter.rs` for the consumer.

ALTER TABLE runs_spans ADD COLUMN exported_at INTEGER NULL;
ALTER TABLE runs_spans ADD COLUMN sampled_in  INTEGER NOT NULL DEFAULT 1;

CREATE INDEX idx_runs_spans_export_pending
  ON runs_spans (exported_at)
  WHERE exported_at IS NULL AND sampled_in = 1;

-- Scrollback rows are flushed from the in-memory ring on pane close,
-- and two flush paths can overlap (waiter finalise vs app-exit
-- shutdown_all, or swarm-term stop() -> kill_pane). Make the flush
-- idempotent at the schema layer: dedupe whatever overlaps already
-- produced, then enforce one row per (pane_id, seq) so the writer can
-- INSERT OR IGNORE.
DELETE FROM pane_lines
WHERE id NOT IN (
  SELECT MIN(id) FROM pane_lines GROUP BY pane_id, seq
);

DROP INDEX idx_pane_lines_pane;
CREATE UNIQUE INDEX idx_pane_lines_pane ON pane_lines(pane_id, seq);

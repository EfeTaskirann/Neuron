-- 0003 — Pane approval banner persistence (③+④).
--
-- The terminal reader extracts `{tool, target, added, removed}` from
-- regex matches against the per-agent awaiting-approval pattern set
-- (`sidecar::terminal::matches_awaiting_approval`). The match blob is
-- serialised to JSON and stamped into this column on every transition
-- to `awaiting_approval`; `commands::terminal::terminal_list` reads it
-- back when materialising `Pane.approval` for the frontend's amber
-- banner strip (`NEURON_TERMINAL_REPORT.md` § Visual contract).
--
-- The column is intentionally NOT cleared when a pane re-enters
-- `running`: a future debug view may want to replay the last seen
-- banner, and `terminal_list` already gates `Pane.approval = None`
-- on the live status. SQLite's `ALTER TABLE … ADD COLUMN` is not
-- idempotent on its own, but the sqlx migrator records the version
-- in `_sqlx_migrations` so re-launching against a migrated DB is a
-- no-op (covered by `db::tests::migrations_are_idempotent`).

ALTER TABLE panes ADD COLUMN last_approval_json TEXT;

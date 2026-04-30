-- 0002 — schema constraint fixes flagged by report.md.
--
-- 1. K7: `mailbox` had no PRIMARY KEY. The frontend used SQLite's
--    implicit `rowid` as a stable React key, but rowid is reusable
--    after a `DELETE` unless the column is `INTEGER PRIMARY KEY
--    AUTOINCREMENT`. Add a real autoincrement id so two emits at the
--    same `ts` get distinct, monotonic, never-reused ids.
--
-- 2. Y1: `runs_spans.parent_span_id` had no `ON DELETE` clause. With
--    `run_id REFERENCES runs(id) ON DELETE CASCADE`, deleting a run
--    cascades into spans — but the self-referential parent FK then
--    has unspecified order. Pin to `ON DELETE CASCADE` so deleting a
--    parent span deterministically removes its children.
--
-- 3. Y16: `runs.status` CHECK accepted only `running|success|error`,
--    so user-driven `runs:cancel` was forced to file as `error`. Add
--    `cancelled` so the runs list can distinguish a user cancel from
--    a real failure.
--
-- SQLite cannot mutate a CHECK constraint, FK list, or PRIMARY KEY in
-- place; the canonical pattern is "create new, copy data, drop old,
-- rename" inside an off-foreign-keys block (per sqlite.org
-- /lang_altertable.html#otheralter §7).

PRAGMA foreign_keys = OFF;

-- ----------------------------------------------------------------- --
-- mailbox: add INTEGER PRIMARY KEY AUTOINCREMENT id
-- ----------------------------------------------------------------- --
CREATE TABLE mailbox_new (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  from_pane TEXT NOT NULL,
  to_pane TEXT NOT NULL,
  type TEXT NOT NULL,
  summary TEXT NOT NULL
);
INSERT INTO mailbox_new (id, ts, from_pane, to_pane, type, summary)
SELECT rowid, ts, from_pane, to_pane, type, summary FROM mailbox;
DROP TABLE mailbox;
ALTER TABLE mailbox_new RENAME TO mailbox;
CREATE INDEX idx_mailbox_ts ON mailbox(ts DESC);

-- ----------------------------------------------------------------- --
-- runs_spans: pin parent_span_id ON DELETE CASCADE
-- ----------------------------------------------------------------- --
CREATE TABLE runs_spans_new (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  parent_span_id TEXT REFERENCES runs_spans(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  type TEXT NOT NULL CHECK (type IN ('llm','tool','logic','human','http')),
  t0_ms INTEGER NOT NULL,
  duration_ms INTEGER,
  attrs_json TEXT NOT NULL DEFAULT '{}',
  prompt TEXT,
  response TEXT,
  is_running INTEGER NOT NULL DEFAULT 0
);
INSERT INTO runs_spans_new
  SELECT id, run_id, parent_span_id, name, type, t0_ms, duration_ms,
         attrs_json, prompt, response, is_running
  FROM runs_spans;
DROP TABLE runs_spans;
ALTER TABLE runs_spans_new RENAME TO runs_spans;
CREATE INDEX idx_spans_run ON runs_spans(run_id);
CREATE INDEX idx_spans_parent ON runs_spans(parent_span_id);

-- ----------------------------------------------------------------- --
-- runs: extend status CHECK to include 'cancelled'
-- ----------------------------------------------------------------- --
CREATE TABLE runs_new (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id),
  workflow_name TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  duration_ms INTEGER,
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('running','success','error','cancelled'))
);
INSERT INTO runs_new
  SELECT id, workflow_id, workflow_name, started_at, duration_ms,
         tokens, cost_usd, status
  FROM runs;
DROP TABLE runs;
ALTER TABLE runs_new RENAME TO runs;
CREATE INDEX idx_runs_started ON runs(started_at DESC);
CREATE INDEX idx_runs_status ON runs(status);

PRAGMA foreign_keys = ON;

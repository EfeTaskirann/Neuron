-- Neuron schema baseline.
--
-- The migration chain 0001_init … 0013_servers_drop_mock_stats was
-- squashed into this single file on 2026-10-18. Every statement below
-- is the schema that chain produced, copied verbatim from sqlite_master
-- (tables, then indexes), followed by the rows the chain seeded.
-- A database that recorded the old chain is moved aside on startup
-- and replaced by a fresh one (see `db::retire_pre_baseline_db`).
--
-- From here on: add NEW numbered migrations; never edit an applied one
-- (sqlx checksums the exact bytes). Migrations are LF-only, pinned in
-- .gitattributes.

-- table: agents
CREATE TABLE agents (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  model TEXT NOT NULL,
  temp REAL NOT NULL,
  role TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

-- table: workflows
CREATE TABLE workflows (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  saved_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

-- table: nodes
CREATE TABLE nodes (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('llm','tool','logic','human','mcp')),
  x INTEGER NOT NULL,
  y INTEGER NOT NULL,
  title TEXT NOT NULL,
  meta TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle'
);

-- table: edges
CREATE TABLE edges (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
  from_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  to_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  active INTEGER NOT NULL DEFAULT 0
);

-- table: servers
CREATE TABLE servers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  by TEXT NOT NULL,
  description TEXT NOT NULL,
  installed INTEGER NOT NULL DEFAULT 0
);

-- table: server_tools
CREATE TABLE server_tools (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  input_schema_json TEXT NOT NULL DEFAULT '{}',
  UNIQUE (server_id, name)
);

-- table: panes
CREATE TABLE panes (
  id TEXT PRIMARY KEY,
  workspace TEXT NOT NULL DEFAULT 'personal',
  agent_kind TEXT NOT NULL,              -- 'claude-code' | 'codex' | 'gemini' | 'shell'
  role TEXT,
  cwd TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'idle',
  pid INTEGER,
  started_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
  closed_at INTEGER
, last_approval_json TEXT);

-- table: pane_lines
CREATE TABLE pane_lines (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  pane_id TEXT NOT NULL REFERENCES panes(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  k TEXT NOT NULL,                       -- 'sys'|'prompt'|'command'|'thinking'|'tool'|'out'|'err'
  text TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

-- table: mailbox
CREATE TABLE "mailbox" (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  ts INTEGER NOT NULL,
  from_pane TEXT NOT NULL,
  to_pane TEXT NOT NULL,
  type TEXT NOT NULL,
  summary TEXT NOT NULL
, kind         TEXT NOT NULL DEFAULT 'note', parent_id    INTEGER, payload_json TEXT NOT NULL DEFAULT '{}');

-- table: runs_spans
CREATE TABLE "runs_spans" (
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
, exported_at INTEGER NULL, sampled_in  INTEGER NOT NULL DEFAULT 1);

-- table: runs
CREATE TABLE "runs" (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id),
  workflow_name TEXT NOT NULL,
  started_at INTEGER NOT NULL,
  duration_ms INTEGER,
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('running','success','error','cancelled'))
);

-- table: settings
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
) WITHOUT ROWID;

-- table: swarm_jobs
CREATE TABLE swarm_jobs (
  id              TEXT    PRIMARY KEY,
  workspace_id    TEXT    NOT NULL,
  goal            TEXT    NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  state           TEXT    NOT NULL,
  retry_count     INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  finished_at_ms  INTEGER
, last_verdict_json TEXT, source TEXT NOT NULL DEFAULT 'fsm') WITHOUT ROWID;

-- table: swarm_stages
CREATE TABLE swarm_stages (
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  idx             INTEGER NOT NULL,
  state           TEXT    NOT NULL,
  specialist_id   TEXT    NOT NULL,
  assistant_text  TEXT    NOT NULL,
  session_id      TEXT    NOT NULL,
  total_cost_usd  REAL    NOT NULL,
  duration_ms     INTEGER NOT NULL,
  created_at_ms   INTEGER NOT NULL, verdict_json TEXT, decision_json TEXT,
  PRIMARY KEY (job_id, idx)
);

-- table: swarm_workspace_locks
CREATE TABLE swarm_workspace_locks (
  workspace_id    TEXT    PRIMARY KEY,
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  acquired_at_ms  INTEGER NOT NULL
) WITHOUT ROWID;

-- table: orchestrator_messages
CREATE TABLE orchestrator_messages (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id    TEXT    NOT NULL,
  role            TEXT    NOT NULL,
  content         TEXT    NOT NULL,
  goal            TEXT,
  created_at_ms   INTEGER NOT NULL
);

-- index: idx_nodes_workflow
CREATE INDEX idx_nodes_workflow ON nodes(workflow_id);

-- index: idx_edges_workflow
CREATE INDEX idx_edges_workflow ON edges(workflow_id);

-- index: idx_mailbox_ts
CREATE INDEX idx_mailbox_ts ON mailbox(ts DESC);

-- index: idx_spans_run
CREATE INDEX idx_spans_run ON runs_spans(run_id);

-- index: idx_spans_parent
CREATE INDEX idx_spans_parent ON runs_spans(parent_span_id);

-- index: idx_runs_started
CREATE INDEX idx_runs_started ON runs(started_at DESC);

-- index: idx_runs_status
CREATE INDEX idx_runs_status ON runs(status);

-- index: idx_runs_spans_export_pending
CREATE INDEX idx_runs_spans_export_pending
  ON runs_spans (exported_at)
  WHERE exported_at IS NULL AND sampled_in = 1;

-- index: idx_swarm_jobs_workspace
CREATE INDEX idx_swarm_jobs_workspace ON swarm_jobs (workspace_id);

-- index: idx_swarm_jobs_state
CREATE INDEX idx_swarm_jobs_state ON swarm_jobs (state);

-- index: idx_swarm_jobs_created
CREATE INDEX idx_swarm_jobs_created ON swarm_jobs (created_at_ms);

-- index: idx_orchestrator_messages_workspace
CREATE INDEX idx_orchestrator_messages_workspace
  ON orchestrator_messages (workspace_id, created_at_ms);

-- index: idx_pane_lines_pane
CREATE UNIQUE INDEX idx_pane_lines_pane ON pane_lines(pane_id, seq);

-- seed rows
-- Seed the three values currently hardcoded in `commands::me::me_get`.
-- `INSERT OR IGNORE` keeps the seed safe across relaunches and
-- preserves any user edit made via `settings:set` after the first
-- launch.
INSERT OR IGNORE INTO settings (key, value) VALUES
  ('user.name',      'Efe Taşkıran'),
  ('user.initials',  'ET'),
  ('workspace.name', 'Personal');

-- WP-W2-02 — initial schema.
-- Covers every entity referenced by the frontend mock fixtures:
--   Neuron Design/app/data.js          (agents, runs, servers, workflows…)
--   Neuron Design/app/terminal-data.js (panes, pane lines, mailbox)
-- Booleans are stored as INTEGER (0|1) per SQLite convention.
-- Timestamps are unix epoch seconds unless suffixed with `_ms`.

-- agents
CREATE TABLE agents (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  model TEXT NOT NULL,
  temp REAL NOT NULL,
  role TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

-- workflows
CREATE TABLE workflows (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  saved_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);

-- nodes (per workflow)
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
CREATE INDEX idx_nodes_workflow ON nodes(workflow_id);

-- edges (per workflow)
CREATE TABLE edges (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
  from_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  to_node TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  active INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_edges_workflow ON edges(workflow_id);

-- runs
CREATE TABLE runs (
  id TEXT PRIMARY KEY,
  workflow_id TEXT NOT NULL REFERENCES workflows(id),
  workflow_name TEXT NOT NULL,           -- denormalized for fast list rendering
  started_at INTEGER NOT NULL,
  duration_ms INTEGER,
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_usd REAL NOT NULL DEFAULT 0,
  status TEXT NOT NULL CHECK (status IN ('running','success','error'))
);
CREATE INDEX idx_runs_started ON runs(started_at DESC);
CREATE INDEX idx_runs_status ON runs(status);

-- spans (OTel-style)
CREATE TABLE runs_spans (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
  parent_span_id TEXT REFERENCES runs_spans(id),
  name TEXT NOT NULL,
  type TEXT NOT NULL CHECK (type IN ('llm','tool','logic','human','http')),
  t0_ms INTEGER NOT NULL,
  duration_ms INTEGER,
  attrs_json TEXT NOT NULL DEFAULT '{}',
  prompt TEXT,
  response TEXT,
  is_running INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_spans_run ON runs_spans(run_id);
CREATE INDEX idx_spans_parent ON runs_spans(parent_span_id);

-- MCP servers
CREATE TABLE servers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  by TEXT NOT NULL,
  description TEXT NOT NULL,
  installs INTEGER NOT NULL DEFAULT 0,
  rating REAL NOT NULL DEFAULT 0,
  featured INTEGER NOT NULL DEFAULT 0,
  installed INTEGER NOT NULL DEFAULT 0
);

-- tools registered by installed MCP servers
CREATE TABLE server_tools (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  description TEXT NOT NULL,
  input_schema_json TEXT NOT NULL DEFAULT '{}',
  UNIQUE (server_id, name)
);

-- terminal panes
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
);

-- ring-buffer scrollback persisted on pane close
CREATE TABLE pane_lines (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  pane_id TEXT NOT NULL REFERENCES panes(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL,
  k TEXT NOT NULL,                       -- 'sys'|'prompt'|'command'|'thinking'|'tool'|'out'|'err'
  text TEXT NOT NULL,
  created_at INTEGER NOT NULL DEFAULT (strftime('%s','now'))
);
CREATE INDEX idx_pane_lines_pane ON pane_lines(pane_id, seq);

-- mailbox (cross-pane events)
CREATE TABLE mailbox (
  ts INTEGER NOT NULL,
  from_pane TEXT NOT NULL,
  to_pane TEXT NOT NULL,
  type TEXT NOT NULL,
  summary TEXT NOT NULL
);
CREATE INDEX idx_mailbox_ts ON mailbox(ts DESC);

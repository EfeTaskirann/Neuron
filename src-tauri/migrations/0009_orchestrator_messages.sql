-- 0009 — Orchestrator persistent chat history (WP-W3-12k2).
--
-- One table backs the per-workspace chat thread between the user and
-- the Orchestrator persona. W3-12k1 shipped the stateless brain;
-- W3-12k3 shipped the chat panel UI; this migration is the storage
-- layer that lets the panel survive a reload AND lets the brain see
-- recent N messages on every `swarm:orchestrator_decide` call.
--
-- Single TEXT `content` column with role-based interpretation:
--
--   * role='user'         — `content` carries the raw user text.
--                           `goal` is NULL.
--   * role='orchestrator' — `content` carries a JSON-serialized
--                           OrchestratorOutcome (action + text +
--                           reasoning packed for round-trip; see
--                           `orchestrator_session::append_orchestrator_message`).
--                           `goal` is NULL.
--   * role='job'          — `content` carries the dispatched job_id;
--                           `goal` carries the refined goal that the
--                           Coordinator FSM was started with.
--
-- The role-aware shape keeps the schema simple at the cost of a
-- per-role parser in the read path. An alternative (dedicated columns
-- per shape) would balloon the schema for marginal gain on what is
-- effectively an append-only conversation log.
--
-- Index on `(workspace_id, created_at_ms)` is the only useful access
-- pattern: "give me the recent N messages for this workspace, oldest
-- first after a DESC + reverse". The frontend never queries by id or
-- by role.
--
-- No FK on `workspace_id` — workspaces are an implicit string-keyed
-- namespace, not a first-class table (mirrors the W3-12b
-- `swarm_jobs.workspace_id` design).

CREATE TABLE orchestrator_messages (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  workspace_id    TEXT    NOT NULL,
  role            TEXT    NOT NULL,
  content         TEXT    NOT NULL,
  goal            TEXT,
  created_at_ms   INTEGER NOT NULL
);

CREATE INDEX idx_orchestrator_messages_workspace
  ON orchestrator_messages (workspace_id, created_at_ms);

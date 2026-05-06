-- 0006 — swarm Coordinator persistence (WP-W3-12b).
--
-- Three tables back the FSM's `JobRegistry` write-through layer:
--
--   * `swarm_jobs` — one row per Coordinator job. Append-only at
--     row-grain (the FSM never deletes jobs); state transitions
--     mutate `state`, `retry_count`, `last_error`, `finished_at_ms`
--     in place. PK is the prefixed-ULID `id` per ADR-0007. Indexed
--     on `workspace_id`, `state`, and `created_at_ms` so the recent-
--     jobs panel (W3-14) can paginate by workspace + recency without
--     a table scan.
--
--   * `swarm_stages` — one row per completed stage. Composite PK
--     `(job_id, idx)` makes the "stage N for job X" lookup index-
--     direct; `ON DELETE CASCADE` keeps trim-policy work (W3-12b+)
--     from leaving stage rows orphaned when their job row is purged.
--
--   * `swarm_workspace_locks` — one row per in-flight workspace.
--     PK is `workspace_id` so the FSM's `try_acquire_workspace`
--     surfaces a unique-constraint violation as the canonical
--     `WorkspaceBusy` signal even at the SQL boundary. CASCADE on
--     `job_id` keeps the FK enforced symmetrically with `swarm_stages`.
--
-- `WITHOUT ROWID` for `swarm_jobs` (string PK) and
-- `swarm_workspace_locks` (string PK) saves one btree level vs. the
-- implicit rowid b-tree. `swarm_stages` keeps rowid because its
-- composite PK ends in an integer (`idx`) — without-rowid would be a
-- marginal pessimization there.
--
-- See `src-tauri/src/swarm/coordinator/store.rs` for the SQL helpers
-- and `src-tauri/src/swarm/coordinator/job.rs` for the in-memory
-- write-through wiring.

CREATE TABLE swarm_jobs (
  id              TEXT    PRIMARY KEY,
  workspace_id    TEXT    NOT NULL,
  goal            TEXT    NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  state           TEXT    NOT NULL,
  retry_count     INTEGER NOT NULL DEFAULT 0,
  last_error      TEXT,
  finished_at_ms  INTEGER
) WITHOUT ROWID;

CREATE INDEX idx_swarm_jobs_workspace ON swarm_jobs (workspace_id);
CREATE INDEX idx_swarm_jobs_state ON swarm_jobs (state);
CREATE INDEX idx_swarm_jobs_created ON swarm_jobs (created_at_ms);

CREATE TABLE swarm_stages (
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  idx             INTEGER NOT NULL,
  state           TEXT    NOT NULL,
  specialist_id   TEXT    NOT NULL,
  assistant_text  TEXT    NOT NULL,
  session_id      TEXT    NOT NULL,
  total_cost_usd  REAL    NOT NULL,
  duration_ms     INTEGER NOT NULL,
  created_at_ms   INTEGER NOT NULL,
  PRIMARY KEY (job_id, idx)
);

CREATE TABLE swarm_workspace_locks (
  workspace_id    TEXT    PRIMARY KEY,
  job_id          TEXT    NOT NULL REFERENCES swarm_jobs(id) ON DELETE CASCADE,
  acquired_at_ms  INTEGER NOT NULL
) WITHOUT ROWID;

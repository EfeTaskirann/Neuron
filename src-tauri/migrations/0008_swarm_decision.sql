-- 0008 — swarm Coordinator brain decision (WP-W3-12f).
--
-- Adds one nullable JSON column:
--
--   * `swarm_stages.decision_json` — populated for the Classify
--     stage only (one CoordinatorDecision per job, on the
--     Scout → Classify → ... edge). NULL for every other stage.
--
-- ALTER TABLE on SQLite is restricted; ADD COLUMN with a nullable
-- default is the only safe op here (matches the WP §"Notes/risks"
-- section). The column is TEXT (raw JSON the FSM serializes via
-- `serde_json::to_string`).

ALTER TABLE swarm_stages ADD COLUMN decision_json TEXT;

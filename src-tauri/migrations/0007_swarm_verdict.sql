-- 0007 — swarm Verdict gate (WP-W3-12d).
--
-- Adds two nullable JSON columns:
--
--   * `swarm_stages.verdict_json` — populated for Review and Test
--     stages (one Verdict per stage). NULL for Scout/Plan/Build.
--
--   * `swarm_jobs.last_verdict_json` — populated when the FSM
--     finalized the job as Failed because a Reviewer or
--     IntegrationTester verdict came back rejected. The Verdict
--     IS the structured error in that case; `last_error` stays
--     NULL (the verdict carries the issue list + summary).
--
-- ALTER TABLE on SQLite is restricted; ADD COLUMN with a nullable
-- default is the only safe op here (matches the Notes/risks
-- section of WP-W3-12d). Both columns are TEXT (raw JSON the FSM
-- serializes via `serde_json::to_string`).

ALTER TABLE swarm_stages ADD COLUMN verdict_json TEXT;
ALTER TABLE swarm_jobs   ADD COLUMN last_verdict_json TEXT;

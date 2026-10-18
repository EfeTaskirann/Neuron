-- MCP catalog: only servers Neuron can launch are listed now, without
-- the design mock's invented install counts, ratings and "featured"
-- curation. The rows themselves follow the bundled catalog (see
-- `db::seed_mcp_servers`).
ALTER TABLE servers DROP COLUMN installs;
ALTER TABLE servers DROP COLUMN rating;
ALTER TABLE servers DROP COLUMN featured;

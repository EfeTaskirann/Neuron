-- 0004 — `settings` key/value table (WP-W3-01).
--
-- Backs the `me:get` user/workspace fields and the W3-08-era
-- Settings route. Keys are dot-namespaced (`user.name`,
-- `user.initials`, `workspace.name`, future `otel.endpoint`,
-- `theme.mode`) so the frontend's "Advanced" panel can group rows
-- by prefix without a separate categorisation column.
--
-- This table is **not** for secrets — provider API keys live in
-- the OS keychain via `crate::secrets`. Keeping the two surfaces
-- separated means `settings:list()` is safe to render verbatim in
-- a debug panel, and a SQLite leak (e.g. an exported backup file)
-- never exposes credentials.
--
-- `WITHOUT ROWID` halves the on-disk footprint for the small
-- key→value rows we expect (a few dozen at most) and makes
-- look-ups by primary key a single B-tree probe rather than
-- two. `value TEXT NOT NULL` forbids storing absence as an empty
-- row — callers use `settings:delete` to clear a key, mirroring
-- the keychain semantics.
--
-- `updated_at` is unix seconds (CAST(strftime('%s','now') AS
-- INTEGER)). The cast is required because `strftime` returns TEXT
-- by default; SQLite would otherwise sort '1234567890' as a
-- string.

CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
) WITHOUT ROWID;

-- Seed the three values currently hardcoded in `commands::me::me_get`.
-- `INSERT OR IGNORE` keeps the seed safe across relaunches and
-- preserves any user edit made via `settings:set` after the first
-- launch.
INSERT OR IGNORE INTO settings (key, value) VALUES
  ('user.name',      'Efe Taşkıran'),
  ('user.initials',  'ET'),
  ('workspace.name', 'Personal');

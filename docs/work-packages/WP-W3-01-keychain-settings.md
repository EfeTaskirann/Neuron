---
id: WP-W3-01
title: OS keychain (Rust) + settings table
owner: TBD
status: not-started
depends-on: []
acceptance-gate: "MCP secrets read via keychain (env var only as test override); `me:get` reads from `settings` table; `secrets:set/has/delete` round-trip through OS keychain."
---

## Goal

Honor Charter §"Hard constraints" #2 ("API keys live in OS
keychain — never plaintext, never `.env` committed") on the Rust
side, and replace the hardcoded values in `commands/me.rs` with a
backing `settings` table so the Settings route (lands in WP-W3-08
era) has somewhere to write user-edited values.

Secrets and settings are the same WP because:

- Both share the "Settings route data sources" theme.
- Both add a Tauri command surface (`secrets:*`, `settings:*`) that
  WP-W3-09 (capabilities tightening) needs to know about.
- Both are short on their own; combining keeps the WP cadence at
  one S-sized package instead of two trivial ones.

The two pieces are kept clearly separated inside the WP — secrets
NEVER touch SQLite; settings NEVER touch the keychain.

## Scope

### 1. Rust `crate::secrets` module

New file `src-tauri/src/secrets.rs` with:

```rust
pub fn get_secret(key: &str) -> Result<Option<String>, AppError>;
pub fn set_secret(key: &str, value: &str) -> Result<(), AppError>;
pub fn has_secret(key: &str) -> Result<bool, AppError>;
pub fn delete_secret(key: &str) -> Result<(), AppError>;
```

Resolution order in `get_secret` (mirrors
`agent_runtime/secrets.py::get_provider_key`):

1. Test/dev env override `NEURON_<KEY>` (uppercased; key chars
   non-alphanumeric replaced with `_`). Used by `cargo test` and
   developer escape hatches; never advertised in user docs.
2. `keyring::Entry::new("neuron", key)?.get_password()`.
   Service name is the constant `"neuron"` — same string the
   Python sidecar uses (`agent_runtime/secrets.py:SERVICE`), so
   one provider key written via `secrets:set('anthropic', ...)`
   is readable by both the Rust MCP runtime and the Python agent
   runtime.

Errors map to `AppError`:

- `keyring::Error::NoEntry` → `Ok(None)` from `get_secret`,
  `Ok(false)` from `has_secret`. NOT an error.
- Every other `keyring::Error` → `AppError::Internal(...)`. The
  message MUST NOT include the secret value (the keyring crate
  does not include it, but log filters in `lib.rs` should still
  exclude any future field that might).

`mod secrets;` declared at the top of `lib.rs` next to the
existing `mod` lines.

Add `keyring = "3"` (or whatever the latest 3.x line is at
authoring time — pin to an exact `=3.x.y`) to
`src-tauri/Cargo.toml` `[dependencies]`. Per Charter risk register
("pin minor versions for stability"), document the exact pin
choice in a code comment.

### 2. Migrate MCP secret reads

Refactor `mcp/registry.rs::resolve_env` (currently lines 228-244)
to call `crate::secrets::get_secret` instead of
`std::env::var`. Keep the env-var override path inside the new
module (as the test/dev escape hatch) so the existing
`requires_secret: "GITHUB_PERSONAL_ACCESS_TOKEN"` flow keeps
working from a developer's shell without writing to the keychain
first.

The error mapping changes meaning slightly:

- Before: missing secret → `AppError::NoApiKey(format!(...))`.
- After: missing secret → still `AppError::NoApiKey` (so the UI
  CTA does not regress); empty string in keychain treated as
  missing (matches the existing `Ok(v) if !v.is_empty()` guard).

### 3. `secrets:*` Tauri commands

New file `src-tauri/src/commands/secrets.rs`:

- `secrets:set(key, value)` → `()`. Writes via
  `crate::secrets::set_secret`. Empty `value` rejected with
  `AppError::InvalidInput`.
- `secrets:has(key)` → `bool`. Wraps `has_secret`. **Never
  returns the value.**
- `secrets:delete(key)` → `()`. Wraps `delete_secret`. Idempotent
  (no-op if absent).

Registered in `lib.rs::specta_builder_for_export` under a `// secrets`
comment block, so `bindings.ts` exports `commands.secretsSet/Has/Delete`.

`secrets:get` is **deliberately NOT a command**. The frontend has
no business reading secret values back; CTAs only need
`secrets:has` + the failure error from the actual consumer
(`mcp:install`, `runs:create`). This keeps the secret value off
the IPC bus.

### 4. Migration `0004_settings.sql`

```sql
CREATE TABLE settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL DEFAULT (CAST(strftime('%s','now') AS INTEGER))
) WITHOUT ROWID;

INSERT OR IGNORE INTO settings (key, value) VALUES
  ('user.name',      'Efe Taşkıran'),
  ('user.initials',  'ET'),
  ('workspace.name', 'Personal');
```

Naming rule: keys are dot-namespaced (`user.name`,
`workspace.name`, future `otel.endpoint`, `theme.mode`, …). The
namespace prefix becomes a fixed enum once W3-09 narrows
capabilities; for now the column is plain TEXT.

Add `settings` to the schema-table count assertion in
`db::tests::migration_creates_all_eleven_tables` (now becomes
twelve — rename test or update the count). The test name has
shipped twice now; rename it to `migration_creates_all_expected_tables`
and let the array grow.

### 5. `settings:*` Tauri commands

New file `src-tauri/src/commands/settings.rs`:

- `settings:get(key)` → `Option<String>`. Plain `SELECT value FROM
  settings WHERE key = ?`.
- `settings:set(key, value)` → `()`. `INSERT ... ON CONFLICT(key)
  DO UPDATE SET value = excluded.value, updated_at = strftime(...)`.
  Empty `value` rejected (use `delete` for absence).
- `settings:delete(key)` → `()`. `DELETE FROM settings WHERE key = ?`.
  Idempotent.
- `settings:list()` → `Vec<(String, String)>`. Returns every
  setting; used by W3-09 Settings route's "Advanced" panel.

### 6. Refactor `commands::me::me_get`

`commands/me.rs` currently hardcodes `initials: "ET"`,
`name: "Efe Taşkıran"`, `workspace: "Personal"`. After this WP:

```rust
let user_name = settings_get(pool, "user.name").await?
    .unwrap_or_else(|| "User".into());
let user_initials = settings_get(pool, "user.initials").await?
    .unwrap_or_else(|| derive_initials(&user_name));
let workspace_name = settings_get(pool, "workspace.name").await?
    .unwrap_or_else(|| "Personal".into());
```

`settings_get` is a private helper inside `commands::me` (or a
shared spot under `commands/util.rs`) that reads one row. The
`derive_initials` fallback handles the case where a user edits
`user.name` but not `user.initials` — first letter of each
whitespace-split word, max 3 chars.

The composite shape (`Me { user, workspace }`) does not change.
Frontend `useMe()` is untouched.

### 7. Tests

- `secrets.rs` unit tests:
  - env-override path: `NEURON_FOO=bar` → `get_secret("foo")` returns `Some("bar")`.
  - empty env-override treated as absent.
  - `has_secret` returns `true` for env-override path.
  - keyring-backed tests use `#[ignore]` (CI does not have a
    keychain) and document the manual run command.
- `commands/secrets.rs`: invalid input rejected; round-trip
  through env-override (so the test can run on CI).
- migration `0004_settings.sql` — counts grow from 11 → 12
  tables; settings seed values present after init.
- `commands/settings.rs`: get/set/delete round-trip; `list()`
  returns ≥3 rows on a freshly-seeded DB.
- `commands::me::me_get` reads from settings table; updating
  `user.name` via `settings:set` is reflected on next `me:get`.

Target test delta: +12 to +18 unit tests (`#[ignore]`d
keychain integration excluded from CI count).

### 8. Bindings

```bash
pnpm gen:bindings
```

Expect `bindings.ts` to gain:

- `commands.secretsSet`, `commands.secretsHas`, `commands.secretsDelete`
- `commands.settingsGet`, `commands.settingsSet`,
  `commands.settingsDelete`, `commands.settingsList`

Verify with `pnpm gen:bindings:check` (the `git diff --exit-code`
guard) AFTER regenerating once.

## Out of scope

- ❌ Settings UI / route (Settings page lands in W3-09 cleanup or
  earlier; this WP only provides the data layer)
- ❌ Per-workspace secrets / multi-tenant secret namespacing
- ❌ Secret rotation policies (manual rotate via `secrets:delete` +
  `secrets:set`)
- ❌ Backup / restore of either secrets or settings
- ❌ Touching the Python sidecar's `secrets.py` — it already does
  the right thing; the Rust side is the gap

## Acceptance criteria

- [ ] `keyring` dep added to `src-tauri/Cargo.toml`, pinned exact
- [ ] `crate::secrets` module exists with the four functions
      named in §1
- [ ] `mcp/registry.rs::resolve_env` calls `crate::secrets::get_secret`
      (no remaining direct `std::env::var` for secret reads in this file)
- [ ] `migrations/0004_settings.sql` created; migration test
      asserts the new table is present and seeded
- [ ] `commands/secrets.rs` + `commands/settings.rs` exist and
      register in `lib.rs::specta_builder_for_export`
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check` passes
- [ ] `commands::me::me_get` reads from settings; updating
      `user.name` via `settings:set` reflects on next `me:get`
- [ ] `secrets:get` IS NOT a command; only the consumer code in
      `crate::secrets::get_secret` ever returns the value
- [ ] All Week-2 tests still pass (regression: 110 + new tests)
- [ ] No `eprintln!` introduced (use `tracing::*` per WP-W2-04)
- [ ] No new `unsafe` block
- [ ] Provider key smoke covers `anthropic` and `openai` only
      (WP-W3-overview.md owner decision 2026-05-01); NO Rust enum
      or const list of provider names — the API stays generic so
      a future WP can add `gemini`/`groq`/`together` by editing
      the Settings UI dropdown only

## Verification commands

```bash
# Rust gate
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Bindings sync
pnpm gen:bindings
pnpm gen:bindings:check

# Frontend gate
pnpm typecheck
pnpm test --run
pnpm lint

# Optional: keychain integration smoke (manual; macOS Keychain /
# Windows Credential Manager / Linux secret-service)
cargo test --manifest-path src-tauri/Cargo.toml -- secrets:: --ignored

# Manual: from a running `pnpm tauri dev` devtools console
#   await invoke('secrets:has', { key: 'anthropic' });        // → false
#   await invoke('secrets:set', { key: 'anthropic', value: 'sk-test' });
#   await invoke('secrets:has', { key: 'anthropic' });        // → true
#   await invoke('secrets:delete', { key: 'anthropic' });
#
#   await invoke('settings:set', { key: 'user.name', value: 'Test User' });
#   const me = await invoke('me:get');
#   me.user.name === 'Test User';
```

## Notes / risks

- **Keyring crate version churn**: 3.x introduced an async API
  that 2.x didn't have. Pin to a 3.x exact and document the choice
  alongside the dep. If 3.x has Linux secret-service issues on
  the dev machine, fall back to `keyring = "=2.3.x"` and note in
  AGENT_LOG.
- **Rust ↔ Python keychain interop**: both sides MUST use service
  name `"neuron"` (lowercase, no version suffix). A typo
  silently splits the keystore so Rust can't read what Python
  wrote and vice versa. Add an integration test (ignored) that
  writes via Rust and reads via Python or vice versa.
- **Tauri capability for `secrets:*`**: today
  `capabilities/default.json` allows `core:default` only. The new
  `secrets:*` and `settings:*` commands ride on tauri-specta's
  invoke handler, which respects only the `core:default` set
  PLUS any tauri-specta-collected commands (which auto-allow when
  registered). W3-09 will explicitly enumerate them; for this WP
  no capability change is required.
- **Migration ordering**: this is `0004_settings.sql`, after
  `0003_panes_approval.sql`. Do NOT reorder existing migrations
  to insert before `0003`. The schema test that counts applied
  migrations (`migrations_are_idempotent`) needs its expected
  count bumped from 3 to 4.
- **Settings versus capabilities**: settings are user-editable
  values stored in SQLite. Capabilities are command-allowlist
  rules from Tauri config. Despite the name overlap, the two
  systems do not interact. W3-09 does not read from `settings` —
  it edits `tauri.conf.json` and `capabilities/default.json`.
- **Backwards-compat shim is forbidden**: do not leave the
  hardcoded values in `commands/me.rs` as a fallback. The
  `unwrap_or_else` defaults inside `me_get` are the only fallback
  layer; if the seed in `0004_settings.sql` ever fails, the test
  catches it.

## Sub-agent reminders

- Read `PROJECT_CHARTER.md` §"Hard constraints" #2 before
  touching the secrets module.
- Read `agent_runtime/secrets.py` so the Rust API surface mirrors
  the Python one — same service name, same env-override pattern,
  same "missing key surfaces as a structured error" semantics.
- Do NOT add a `secrets:get` command. The owner has explicitly
  decided the secret value never crosses the IPC boundary.
- Do NOT change the wire shape of `Me`, `User`, or `Workspace` in
  `models.rs`. Only the data source changes.
- Per AGENTS.md: one WP = one commit. Do not split into "secrets
  commit" + "settings commit" — they share migration tests and
  bindings regeneration, so atomic commit is cleaner.

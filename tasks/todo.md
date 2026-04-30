# WP-W2-05 — MCP server registry — execution plan

## Plan checklist

### A. Module scaffold
- [ ] Create `src-tauri/src/mcp/mod.rs` exposing `client`, `registry`, `manifests` submodules
- [ ] Create `src-tauri/src/mcp/manifests/` (directory) and 6 JSON files: filesystem, github, postgres, browser, slack, vector-db
- [ ] Wire `pub mod mcp;` into `src-tauri/src/lib.rs`

### B. JSON-RPC client (newline-delimited)
- [ ] `src-tauri/src/mcp/client.rs` — newline-delimited JSON-RPC 2.0 over stdio
- [ ] Implement `initialize`, `notifications/initialized`, `tools/list`, `tools/call`, `ping`
- [ ] Spawn helper that resolves `npx` (or `npx.cmd` on Windows) and starts a child
- [ ] Send/receive correlated by `id` per JSON-RPC 2.0
- [ ] Unit tests for codec round-trip

### C. Registry
- [ ] `src-tauri/src/mcp/registry.rs` — manifest loading, install/uninstall flow, tool persistence
- [ ] Load manifests via `include_str!` and parse on demand
- [ ] `install(pool, id)` → spawn → handshake → tools/list → persist tools, flip flag
- [ ] `uninstall(pool, id)` → delete server_tools rows, flip flag
- [ ] `list_tools(pool, id)` → query server_tools
- [ ] `call_tool(server_id, name, args)` → spawn server (or ephemeral exec), handshake, tools/call
- [ ] Unit tests against in-memory pool for CRUD

### D. Models/error
- [ ] Add `Tool` and `CallToolResult` types in `src-tauri/src/models.rs`
- [ ] Add `McpProtocol` and `McpServerSpawnFailed` variants to `AppError` in `src-tauri/src/error.rs`

### E. Commands
- [ ] Replace `mcp_install` / `mcp_uninstall` stubs in `src-tauri/src/commands/mcp.rs`
- [ ] Add `mcp_list_tools` command
- [ ] Add `mcp_call_tool` command
- [ ] Preserve `mcp:installed` / `mcp:uninstalled` events
- [ ] Update tests

### F. Seed
- [ ] Extend `src-tauri/src/db.rs` with `seed_mcp_servers(&pool)` function
- [ ] Call it from `db::init` after `seed_demo_workflow`
- [ ] Idempotent INSERT OR IGNORE per row, derived from manifests
- [ ] Unit test for idempotency

### G. lib.rs wiring
- [ ] Register `mcp_list_tools` and `mcp_call_tool` in `specta_builder_for_export`
- [ ] No new sidecar startup (lazy spawn)

### H. Bindings + offline cache
- [ ] Run `cargo run --bin export-bindings` — assert `mcpListTools` and `mcpCallTool` appear

### I. Documentation
- [ ] Add `src-tauri/MCP.md` (small note) — version pin + npx requirement

### J. Verification gates
- [ ] cargo check
- [ ] cargo test (target ≥ 56 + new MCP tests, plus 1-2 ignored integrations)
- [ ] cargo run --bin export-bindings
- [ ] pnpm typecheck
- [ ] pnpm lint
- [ ] pnpm test --run
- [ ] git status clean (only allowlist paths)

### K. Commit
- [ ] feat: add MCP client + registry with Filesystem server (WP-W2-05)

## Decisions / notes

- MCP wire format = newline-delimited JSON (NDJSON) per the MCP spec, NOT length-prefixed (which is our internal sidecar's framing).
- For `mcp:install` / `mcp:callTool`, Filesystem is fully wired: `npx -y @modelcontextprotocol/server-filesystem <root>`.
- Other 5 manifests have a clear "manifest not yet wired" error path so install on them is a controlled failure rather than a crash.
- For `mcp:install('filesystem')`, the workspace root passed to the server defaults to the Tauri `app_data_dir` for safety; in `cargo test` we accept any tempdir.
- Server processes spawned for install spawn fresh per call (no long-lived MCP process pooling) — Week 3 may add pooling.
- All Tauri events use `:` (colon) per ADR-0006 carryover; `.` panics on Tauri 2.10.

## Review section

### Outcome

WP-W2-05 implemented end-to-end. All 8 acceptance criteria addressed
either by unit tests (DB-backed CRUD, idempotency, persistence,
seeding) or by an `#[ignore]`d integration test that spawns the real
`@modelcontextprotocol/server-filesystem` against a tempdir,
performs `tools/list` (≥5 tools), and calls `tools/call read_text_file`.

### Changes by group

- `src-tauri/src/mcp/` — new module: `mod.rs`, `client.rs`,
  `manifests.rs`, `registry.rs`, plus 6 JSON manifests
- `src-tauri/src/commands/mcp.rs` — replaced WP-W2-03 stubs with real
  install/uninstall and added `mcp_list_tools` + `mcp_call_tool`
- `src-tauri/src/db.rs` — added `seed_mcp_servers` called from
  `db::init`
- `src-tauri/src/error.rs` — added `McpProtocol` and
  `McpServerSpawnFailed` variants
- `src-tauri/src/models.rs` — added `Tool`, `ToolContent`,
  `CallToolResult` IPC types
- `src-tauri/src/lib.rs` — registered the two new commands and the
  `mcp` module
- `src-tauri/MCP.md` — version pin + npx/runtime requirement note
- `app/src/lib/bindings.ts` — regenerated, includes `mcpListTools`,
  `mcpCallTool`, `Tool`, `ToolContent`, `CallToolResult`

### Test count

- 75 passing (up from 56 prior to WP-W2-05) — 19 new unit tests
- 2 ignored (up from 1 prior) — 1 new integration test

### Notes / decisions

- MCP wire format is **newline-delimited JSON** per the spec, not the
  length-prefixed framing used by the WP-W2-04 LangGraph sidecar.
- `mcp:callTool` takes `argsJson: string` (caller does
  `JSON.stringify(args)`) rather than a typed `Value`. The reason is
  specta's `serde_json::Value` representation produces a broken TS
  type. A string-encoded JSON keeps `bindings.ts` clean and the
  wire shape explicit.
- 4 of the 6 seeded servers (postgres, browser, slack, vector-db) are
  catalog-only stubs in Week 2 — installing them surfaces a clear
  `mcp_server_spawn_failed` error rather than silently flipping the
  flag. Filesystem and GitHub are fully wired.
- Filesystem MCP server works against the per-app data dir; GitHub
  needs `GITHUB_PERSONAL_ACCESS_TOKEN` in env (Week 3 will route
  through the OS keychain).

---
id: WP-W2-05
title: MCP server registry
owner: TBD
status: not-started
depends-on: [WP-W2-03]
acceptance-gate: "Install/uninstall flow persists; agent runtime can list installed tools"
---

## Goal

Real MCP server install/uninstall using the Anthropic MCP spec. Servers register their tools after install; agent runtime (WP-W2-04) can call those tools. Two seeded servers: Filesystem, GitHub.

## Scope

- Add MCP client integration to `src-tauri/`:
  - Either a published `mcp-rs` crate if available, or a minimal in-house client implementing stdio transport for the MCP protocol (initialize, tools/list, tools/call)
- New module: `src-tauri/src/mcp/` with:
  - `client.rs` — stdio transport, JSON-RPC 2.0
  - `registry.rs` — install/uninstall flow, tool registration
- `mcp:install(id)`:
  - Looks up server manifest by id
  - Spawns the server process to run `tools/list`
  - Persists `installed=1` and inserts rows in `server_tools`
  - Returns updated `Server`
- `mcp:uninstall(id)` flips flag, deletes `server_tools` rows
- `mcp:listTools(serverId)` (new command) returns tools for an installed server (used by agent runtime)
- `mcp:callTool(serverId, name, args)` (new command) executes a tool call (used by agent runtime via Rust → MCP server stdio)
- Seeded servers at first run via migration `0002_seed_mcp.sql`:
  - Filesystem (id `filesystem`, by `Anthropic`, featured)
  - GitHub (id `github`, by `GitHub`, featured)
  - Plus 4 non-featured: PostgreSQL, Browser, Slack, Vector DB (matching prototype mock)

## Out of scope

- Custom server URLs (Week 3)
- Sandbox isolation (security follow-up — Week 3)
- Marketplace install from third-party registry (Week 3)
- Server auto-update (Week 3)

## Acceptance criteria

- [ ] `mcp:install('filesystem')` succeeds without API key (Filesystem doesn't need one)
- [ ] `mcp:list()` shows `installed=true` for filesystem
- [ ] `server_tools` populated for installed server (at least 5 tools for Filesystem)
- [ ] `mcp:listTools('filesystem')` returns the tools array
- [ ] `mcp:callTool('filesystem', 'read_file', { path: 'README.md' })` returns file contents (smoke test in dev mode only — sandboxed in Week 3)
- [ ] `mcp:uninstall('filesystem')` removes tools, flips flag
- [ ] State persists across app restarts (DB-backed)
- [ ] Servers seeded at first run (idempotent migration)

## Verification commands

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- mcp
# manual:
#   await invoke('mcp:install', { id: 'filesystem' })   // succeeds
#   await invoke('mcp:list')                            // filesystem.installed === true
#   await invoke('mcp:listTools', { serverId: 'filesystem' })  // tools.length >= 5
#   restart app
#   await invoke('mcp:list')                            // filesystem still installed
#   await invoke('mcp:uninstall', { id: 'filesystem' }) // succeeds
```

## Notes / risks

- MCP spec churn — pin to a specific version. Document in README.
- GitHub server requires a personal access token. Configure via OS keychain (same pattern as WP-04 API keys). Surface a clear error if missing.
- Filesystem server in Week 2 has NO sandboxing. Document the risk; add sandboxing in Week 3 (`--allowed-paths` flag).
- Seeded server manifests live in `src-tauri/src/mcp/manifests/` as JSON. Loaded by migration `0002_seed_mcp.sql` via Rust seed function (not raw SQL — too brittle).
- The in-house MCP client should support: `initialize`, `notifications/initialized`, `tools/list`, `tools/call`, `ping`. Subscriptions and resources are out-of-scope for Week 2.

## Sub-agent reminders

- Do NOT introduce a JS-side MCP client. Frontend calls `mcp:*` commands only.
- Do NOT inline MCP server binaries — use `npx @modelcontextprotocol/server-filesystem` style spawn for built-in servers.

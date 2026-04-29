# MCP integration notes

This file lives at `src-tauri/MCP.md` and pins the MCP-related runtime
contracts for Neuron's Week-2 backend. Cross-reference with
`PROJECT_CHARTER.md` § "Tech stack" and `docs/work-packages/WP-W2-05-mcp-registry.md`.

## Spec version pin

Neuron speaks MCP protocol version **`2024-11-05`**. Bumps go through
an ADR per the Charter risk register ("Anthropic MCP spec churn").
The constant is exported as `crate::mcp::client::MCP_PROTOCOL_VERSION`
and sent in every `initialize` handshake.

## Runtime requirement: `npx`

Built-in MCP servers are spawned as Node.js child processes via `npx`.
The Tauri shell does not bundle a Node.js runtime, so the host machine
must have:

- Node.js 18+ on `PATH`
- `npx` resolvable as `npx` (Unix) or `npx.cmd` (Windows)

A missing `npx` surfaces to the frontend as
`AppError::McpServerSpawnFailed` (`kind = "mcp_server_spawn_failed"`),
not as a silent "Install" toggle that does nothing.

## Bundled manifests

Six servers ship as JSON manifests under `src-tauri/src/mcp/manifests/`:

| id            | featured | spawn wired | secret env var                  |
|---------------|---------:|:-----------:|---------------------------------|
| `filesystem`  | yes      | yes         | —                               |
| `github`      | yes      | yes         | `GITHUB_PERSONAL_ACCESS_TOKEN`  |
| `postgres`    | no       | no (stub)   | `POSTGRES_CONNECTION_STRING`    |
| `browser`     | no       | no (stub)   | —                               |
| `slack`       | no       | no (stub)   | `SLACK_BOT_TOKEN`               |
| `vector-db`   | no       | no (stub)   | `QDRANT_URL`                    |

"stub" means the manifest is catalog-only: `mcp:list` returns the row,
but `mcp:install` raises `McpServerSpawnFailed`. The full pipeline for
those four servers lands alongside Week-3 sandbox isolation.

## Filesystem server: no sandboxing in Week 2

The Filesystem MCP server runs `npx -y @modelcontextprotocol/server-filesystem
<root>`, where `<root>` is the per-app data dir resolved by Tauri.
Week-2 has **no** sandboxing on top of the server's own checks — a
malicious tool call that escapes the root would land directly on the
host filesystem. Week-3 adds a `--allowed-paths` constraint and a
deny-list filter (per WP-W2-05 §"Notes / risks").

## Secrets

API keys and PATs come from `std::env::var(...)` in Week-2. WP-W2-04's
keychain wiring lands in Week-3, after which `resolve_env` will route
through the OS keychain per Charter §"Hard constraints" #2.

## Why no session pool

Each `mcp:callTool` re-spawns the server, performs the handshake, runs
the tool, and shuts down. That is slow (npx cold-start ≈ 2-5s on a
warm cache) but trivially auditable. Pooling is a Week-3 follow-up.

## Frontend contract

Generated bindings (`app/src/lib/bindings.ts`) export typed wrappers:

```ts
commands.mcpList()
commands.mcpInstall(id)
commands.mcpUninstall(id)
commands.mcpListTools(serverId)
commands.mcpCallTool(serverId, name, args)
```

`mcp:installed` and `mcp:uninstalled` fire as Tauri events with the
updated `Server` row payload.

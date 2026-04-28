---
id: WP-W2-03
title: Tauri command surface
owner: TBD
status: not-started
depends-on: [WP-W2-02]
acceptance-gate: "All commands callable from frontend; types generated; cargo + pnpm tests pass"
---

## Goal

Define the Tauri commands the frontend will use, generate TypeScript bindings via `tauri-specta`, and provide unit tests for each command. No real LangGraph / MCP / PTY — those land in WP-04/05/06. This WP only defines the API surface against the WP-02 schema with stub implementations where real impl is later.

## Scope

- Add `tauri-specta` + `specta` to `src-tauri/Cargo.toml`
- Define commands listed below in `src-tauri/src/commands/{agents,runs,workflows,mcp,terminal,mailbox}.rs`
- Each command reads/writes via the WP-02 sqlx pool
- Generate `app/src/lib/bindings.ts` via `specta::ts::export` at build time (or via a build script)
- Wire commands into `tauri::Builder::default().invoke_handler(tauri::generate_handler![...])`
- Unified `AppError` type that serializes to `{ kind: string, message: string }`

## Command list

```
// agents
agents:list      → Agent[]
agents:get       (id: string) → Agent
agents:create    (input: AgentCreateInput) → Agent
agents:update    (id: string, patch: AgentPatch) → Agent
agents:delete    (id: string) → void

// workflows
workflows:list   → Workflow[]
workflows:get    (id: string) → { workflow: Workflow; nodes: Node[]; edges: Edge[] }

// runs
runs:list        (filter?: RunFilter) → Run[]
runs:get         (id: string) → { run: Run; spans: Span[] }
runs:create      (workflowId: string) → Run            // STUB — inserts row, no execution
runs:cancel      (id: string) → void

// MCP
mcp:list         → Server[]
mcp:install      (id: string) → Server                  // STUB — flips installed flag only
mcp:uninstall    (id: string) → Server

// terminal
terminal:list    → Pane[]
terminal:spawn   (input: PaneSpawnInput) → Pane         // STUB — inserts row, no PTY
terminal:kill    (id: string) → void

// mailbox
mailbox:list     (sinceTs?: number) → MailboxEntry[]
mailbox:emit     (entry: MailboxEntryInput) → MailboxEntry
```

Total: 17 commands.

## Acceptance criteria

- [ ] All 17 commands compiled and registered in the invoke handler
- [ ] `app/src/lib/bindings.ts` generated and committed (do NOT hand-edit)
- [ ] `pnpm typecheck` passes referencing `bindings.ts`
- [ ] Each command has a `#[cfg(test)]` unit test (happy path + 1 error path = at least 34 tests)
- [ ] Frontend can call `await invoke('agents:list')` and receive `[]` (DB empty seed)
- [ ] AppError serializes to the documented shape for at least one error path per namespace
- [ ] No mutation of frontend mock files

## Verification commands

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- commands
pnpm typecheck
# manual smoke test in tauri dev devtools console:
#   await window.__TAURI__.invoke('agents:list')          → []
#   await window.__TAURI__.invoke('workflows:list')       → []
#   await window.__TAURI__.invoke('mcp:list')             → []
#   await window.__TAURI__.invoke('terminal:list')        → []
#   await window.__TAURI__.invoke('runs:list')            → []
#   await window.__TAURI__.invoke('agents:get', { id: 'nope' })   → AppError { kind: 'not_found', ... }
```

## Notes / risks

- Naming: kebab-case with colon namespace (`agents:list`). Configure Rust handler with `#[tauri::command(rename_all = "camelCase")]` for arg ergonomics. Note Tauri commands cannot have a colon in the function name; use a name attribute: `#[tauri::command(name = "agents:list")]` if supported, else use the renamer in `invoke_handler!`.
- Errors: return `Result<T, AppError>`. AppError variants: `NotFound`, `Conflict`, `InvalidInput`, `DbError`, `Internal`. Serialized form: `{ "kind": "not_found", "message": "Agent abc not found" }`.
- Stubs in this WP:
  - `runs:create` inserts a row with `status='running'` and no spans. WP-04 makes it real.
  - `mcp:install` only flips `installed=1`. WP-05 adds tool registration.
  - `terminal:spawn` inserts a pane row with `status='idle'`. WP-06 adds the PTY.
- `tauri-specta` may require an `export!` macro at build time. If the integration is awkward, fall back to manually writing `bindings.ts` from a known type module — but document the choice.

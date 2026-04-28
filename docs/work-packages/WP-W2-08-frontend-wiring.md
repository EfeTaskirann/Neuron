---
id: WP-W2-08
title: Frontend mock → real wiring
owner: TBD
status: not-started
depends-on: [WP-W2-03, WP-W2-04, WP-W2-05, WP-W2-06, WP-W2-07]
acceptance-gate: "All routes render real backend data; no remaining window.NeuronData reads in production paths; prototype dirs deleted"
---

## Goal

Replace `window.NeuronData` and `window.NeuronTerminalData` reads with TanStack Query hooks invoking Tauri commands. Component DOM stays unchanged; only the data source changes (per ADR-0005). At the end of this WP, `Neuron Design/` and `neuron-docs/` are deleted; `app/src/` is the canonical home for all frontend code.

## Scope

### 1. Setup

- Add `@tanstack/react-query` v5 to `app/package.json`
- Create `app/src/lib/queryClient.ts` (default 30s stale, 5min gc, retry once)
- Wrap `<App>` in `<QueryClientProvider>` in `main.tsx`

### 2. Hooks

Create `app/src/hooks/`:

- `useMe.ts` → `{ user, workspace }` (combines `data.user` + `data.workspace` — synthesize from a single `me:get` command added in this WP)
- `useAgents.ts` → `agents:list`
- `useAgent(id)` → `agents:get`
- `useRuns(filter)` → `runs:list`
- `useRun(id)` → `runs:get` + subscribes to `run.{id}.span` events
- `useServers.ts` → `mcp:list`
- `useWorkflow(id)` → `workflows:get` (returns `{ workflow, nodes, edges }`)
- `useWorkflows.ts` → `workflows:list`
- `usePanes.ts` → `terminal:list` + subscribes to `pane.{id}.line` events for active pane
- `useMailbox.ts` → `mailbox:list` (polling every 2s)

Mutations:

- `useAgentCreate`, `useAgentUpdate`, `useAgentDelete`
- `useRunCreate`
- `useMcpInstall`, `useMcpUninstall`
- `useTerminalSpawn`, `useTerminalKill`, `useTerminalWrite`

All mutations call the matching `*:create/update/delete/install/...` command and invalidate the relevant list query on success.

### 3. Component migration (per ADR-0005)

| Component | Old data source | New hook |
|---|---|---|
| `Sidebar.tsx` | `data.user`, `data.workspace` | `useMe()` |
| `AgentsRoute.tsx` | `data.agents` | `useAgents()` |
| `RunsRoute.tsx` | `data.runs` | `useRuns()` |
| `MCPRoute.tsx` (server cards + rows) | `data.servers` | `useServers()` |
| `Canvas.tsx` | hardcoded NODES/EDGES | `useWorkflow('daily-summary')` |
| `RunInspector.tsx` | hardcoded SPANS | `useRun(currentRunId)` |
| `TerminalRoute.tsx` | `NeuronTerminalData` | `usePanes()` + `useMailbox()` |

Component DOM and class names DO NOT CHANGE. Only the data acquisition line.

### 4. Empty + error states

- Wrap each route in `<ErrorBoundary>` (a small component that catches query errors and shows a retry button)
- Empty list states already styled in prototype (`.agent-card.add`, etc.) — ensure they render when `data.length === 0`

### 5. Migration of existing assets

- Move `Neuron Design/colors_and_type.css` → `app/src/styles/colors_and_type.css`
- Move `Neuron Design/app/styles.css` → `app/src/styles/styles.css`
- Move `Neuron Design/app/app.css` → `app/src/styles/app.css`
- Move `Neuron Design/app/terminal.css` → `app/src/styles/terminal.css`
- Move `Neuron Design/app/assets/icons/` → `app/src/assets/icons/`
- Move `Neuron Design/app/assets/brandmark.svg` → `app/src/assets/brandmark.svg`
- Move `Neuron Design/app/fonts/` → `app/src/assets/fonts/`
- Convert `Neuron Design/app/icons.jsx` → `app/src/components/icons.tsx` (port to TS, keep API)
- Convert `Neuron Design/app/canvas.jsx` → `app/src/routes/Canvas.tsx`
- Convert `Neuron Design/app/inspector.jsx` → `app/src/routes/RunInspector.tsx`
- Convert `Neuron Design/app/shell.jsx` → `app/src/components/AppShell.tsx`
- Convert `Neuron Design/app/routes.jsx` → individual route files in `app/src/routes/`
- Convert `Neuron Design/app/terminal.jsx` → `app/src/routes/Terminal.tsx`

### 6. Cleanup (final acceptance gate)

- Delete `Neuron Design/` directory
- Delete `neuron-docs/` directory
- Update `.gitignore` if needed (no leftover prototype paths)
- Update `Neuron App.html` references — file is gone; new entry is `app/index.html`

### 7. xterm.js for terminal

Add `xterm` to `app/package.json`. Replace prototype's plain line rendering with xterm in `Terminal.tsx` for ANSI support. Wire `terminal:write` → xterm.onData, `terminal:lines` event → xterm.write.

## Acceptance criteria

- [ ] All routes render real backend data (DB seeded with WP-04/05/06 fixtures)
- [ ] Empty `agents` table → AgentsRoute shows only the dashed "+ New agent" card
- [ ] Backend error (e.g., `agents:list` returns AppError) → ErrorBoundary shows retry button
- [ ] Creating an agent (mutation) instantly reflects in the agents list (cache invalidation works)
- [ ] Run started in agent runtime appears in inspector with live span updates (`run.{id}.span` events)
- [ ] No remaining references to `window.NeuronData` / `window.NeuronTerminalData` in `app/src/`
  - Verify with: `grep -r "window.Neuron" app/src/` returns nothing
- [ ] `Neuron Design/` and `neuron-docs/` directories DELETED
- [ ] `colors_and_type.css` lives at `app/src/styles/colors_and_type.css`
- [ ] `pnpm typecheck` and `pnpm test --run` pass
- [ ] `cargo test --manifest-path src-tauri/Cargo.toml` still passes
- [ ] Manual smoke test (below) passes

## Verification commands

```bash
pnpm typecheck
pnpm test --run
cargo check --manifest-path src-tauri/Cargo.toml
pnpm tauri dev
# manual smoke:
# 1. Workflows route: canvas renders with 6 nodes, dot-grid bg, minimap visible
# 2. Agents route: 4 cards from seed fixture + "+ New agent" dashed card
# 3. Click "+ New agent" → fill form → save → card appears immediately
# 4. Runs route: click "Run" on a workflow → spans appear in inspector live
# 5. Marketplace route: click Install on Filesystem → state persists, button changes to Installed
# 6. Terminal route: spawn 2x2 panes, type in each, ANSI colors render
# 7. Restart app → all state preserved (agents, mcp, runs history, panes scrollback)
# 8. Settings → Appearance → toggle Dark/Light → persists across restart
# 9. grep -r "window.Neuron" app/src/   # should return 0 matches
# 10. ls Neuron\ Design neuron-docs   # should error: No such file or directory
```

## Migration pattern (per ADR-0005)

For each component:
1. Identify `data.X` reads — list `X` keys
2. Find matching backend command (e.g., `data.agents` → `agents:list`)
3. Add hook in `app/src/hooks/useX.ts`
4. Diff:
   ```diff
   -const data = window.NeuronData;
   -const items = data.X;
   +const { data: items = [], isLoading, isError } = useX();
   ```
5. Wrap route in `<ErrorBoundary>` and add empty state
6. Snapshot test for shape parity (`app/src/__tests__/`)

## Risks

- Component code uses `data.X` deeply — destructuring everywhere needs care; do small commits per route
- Some shape fields unused in component might be missing in backend — diff actively at the bindings level
- Seed data for "looks correct" smoke test must match prototype's vibe — copy values from `Neuron Design/app/data.js` into a Rust seed function executed by migration `0003_seed.sql` BEFORE this WP deletes the prototype directory
- xterm.js integration on Windows ConPTY: extra care for resize sequences; test with `pnpm tauri dev` not just hot reload

## Sub-agent reminders

- This is the LARGEST WP. Do it in commits per route, not one big commit.
- Read `ADR-0005` BEFORE starting — strict pattern.
- Do NOT change component class names or DOM structure. Only data source.
- Do NOT delete `Neuron Design/` until ALL components migrated and acceptance criteria pass.
- After deletion, the only surviving reference is the design-system-spec.md (root) and AGENT_LOG.md entries.

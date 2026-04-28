# Neuron Terminal — Architecture Report

**Version:** 1.0
**Owner:** Efe Taşkıran
**Last updated:** 2026-04-28
**Status:** Reference for WP-W2-06

## What is the Terminal route?

A multi-pane terminal-like surface where each pane runs an agent (Claude Code, Codex CLI, Gemini CLI) or a plain shell. Panes communicate via a shared mailbox. Users see live agent state at a glance via colored borders and status pills.

The Week 1 prototype renders a snapshot ("Daily summary scenario") with mock data. Week 2 wires real PTY processes via `portable-pty` (Rust sidecar).

## Current state (mock)

- Source: `Neuron Design/app/terminal-data.js` — `window.NeuronTerminalData`
- Renderer: `Neuron Design/app/terminal.jsx` — `TerminalRoute`
- Styles: `Neuron Design/app/terminal.css`

## Data shape (canonical — backend must match exactly)

```typescript
type NeuronTerminalData = {
  workspace: { name: string; panes: number; layout: '1' | '2v' | '2h' | '2x2' | '3x4' };
  agents: Record<string, {
    name: string;
    accent: 'violet' | 'emerald' | 'amber' | 'sky' | 'slate';
    icon: 'claude' | 'openai' | 'gemini' | 'shell';
  }>;
  panes: Pane[];
  mailbox: MailboxEntry[];
};

type Pane = {
  id: string;                    // e.g. "p1"
  agent: string;                 // key into agents
  role: string | null;           // "builder" | "reviewer" | "test-runner" | null
  cwd: string;                   // absolute or ~ path
  status: 'idle' | 'starting' | 'running' | 'awaiting_approval' | 'success' | 'error';
  pid: number;
  uptime: string;                // human-readable, e.g. "12m 04s" — backend computes
  tokensIn: number | null;
  tokensOut: number | null;
  costUsd: number | null;
  approval?: { tool: string; target: string; added: number; removed: number };
  lines: { k: 'sys' | 'prompt' | 'command' | 'thinking' | 'tool' | 'out' | 'err'; text: string; inline?: string }[];
  blocks: { id: string; cmd: string; exit: number | null; dur: number | null; status: string }[];
};

type MailboxEntry = {
  ts: string;          // human-readable, e.g. "12m 02s"
  from: string;        // pane id
  to: string;          // pane id
  type: 'task:done' | 'task:failed' | 'request:review' | string;
  summary: string;
};
```

## Visual contract

- Pane border color = status color (CSS custom prop `--st-color`)
- Running pane = subtle ambient glow + breathe animation (1600ms)
- Active pane = bright pulsing border (violet/emerald/amber/rose by status)
- Approval banner = amber strip when `pane.approval` is non-null
- Status bar = workspace name + pill counts + layout switcher

All animations are CSS-only. `data-motion="off"` selector disables them globally.

## Backend integration plan (WP-W2-06)

### Goals

1. Spawn real shell processes per pane
2. Stream stdout/stderr to frontend incrementally
3. Persist pane lifecycle in SQLite
4. Detect agent kind from process command (claude-code, codex, gemini-cli, shell) and tag pane with the right `agent` key
5. Mailbox messages are app-level events, NOT raw PTY data — separate channel

### Default shells

| Platform | Default `cmd` |
|---|---|
| Windows | `pwsh.exe` if available, else `powershell.exe` |
| macOS | `$SHELL` (typically `zsh`) |
| Linux | `$SHELL` (typically `bash`) |

### Tauri commands (defined in WP-W2-03, real impl in WP-W2-06)

- `terminal:list` → `Pane[]` from DB
- `terminal:spawn(opts: { cwd: string; cmd?: string; cols?: number; rows?: number; agent?: string })` → `Pane`
- `terminal:write(paneId, data)` → write to PTY stdin
- `terminal:resize(paneId, cols, rows)` → SIGWINCH equivalent
- `terminal:kill(paneId)` → SIGTERM, then SIGKILL after 5s
- `terminal:lines(paneId, sinceSeq)` → ring-buffer scrollback (DB-backed)
- `mailbox:since(ts)` → `MailboxEntry[]`

### Streaming

`terminal:spawn` returns immediately; output streams via Tauri events:

```rust
// rust
window.emit(&format!("pane.{}.line", pane_id), Line { k, text })?;
```

```ts
// frontend
listen<Line>(`pane.${paneId}.line`, e => append(e.payload));
```

### State machine

```
        spawn
   ┌───────────┐
   ▼           │
 idle ──► starting ──► running ──► awaiting_approval ──► running ──► success
                                                     └─► error
```

`awaiting_approval` is detected via stdout pattern matching against agent-specific patterns. For Week 2, simple regex on the last 5 lines:

| Agent | Detection regex |
|---|---|
| claude-code | `/Approve.*\?$/m` or `/^Tool: .* needs approval/m` |
| codex | `/Apply this patch\? \[y\/n\]/m` |
| gemini | `/^\[awaiting\]/m` |
| shell | (never enters this state) |

Week 3 may replace regex with structured exit codes / stdout markers.

### Ring buffer

5,000 lines per pane. When exceeded, oldest 1,000 dropped. On pane close, last 5,000 lines persisted to DB (`pane_lines` table). Restored on app restart.

## Mailbox semantics (Week 2 minimum)

Mailbox is an append-only event log per workspace, NOT real IPC between panes. When pane A finishes a task, it emits `task:done` to the mailbox via:

- Frontend → `invoke('mailbox:emit', { from: 'p1', to: 'p2', type: 'task:done', summary: '...' })`
- Or sidecar (LangGraph) emits structured events that Rust forwards to the mailbox table

Pane B reads the mailbox via `useMailbox()` polling every 2s. Real cross-pane orchestration (e.g., agent-to-agent autonomous handoff) is Week 3.

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Windows ConPTY column-resize deadlock | Medium | Throttle resize events; document limitation |
| Long-output processes consuming RAM | Medium | Ring buffer cap; `pane_lines` flush every 1k lines |
| Killing pane mid-write loses last bytes | Low | Accept; not safety-critical |
| Agent-CLI process discovery races | Low | For Week 2, only spawn what user explicitly requests; auto-detect deferred |
| ANSI escape sequence accumulation | Medium | Strip CSI cursor-control before storing in DB; preserve for live render |

## Out of scope (Week 2)

- ❌ ssh / mosh remote panes
- ❌ Pane recording / replay
- ❌ Tmux/Zellij interop
- ❌ Auto-attach to existing PTY processes
- ❌ Cross-pane autonomous handoff (Week 3)
- ❌ Search across all pane scrollback (Week 3)

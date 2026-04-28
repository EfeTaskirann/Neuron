---
id: WP-W2-06
title: Terminal PTY sidecar
owner: TBD
status: not-started
depends-on: [WP-W2-03]
acceptance-gate: "terminal:spawn opens a real shell; output streams to frontend; 4 panes work in 2x2"
---

## Goal

Replace WP-W2-03 stubs with real PTY processes via `portable-pty` Rust crate. Stream stdout/stderr to frontend incrementally. Persist pane state in DB. Reference: `NEURON_TERMINAL_REPORT.md`.

## Scope

- Add `portable-pty` to `src-tauri/Cargo.toml`
- Module: `src-tauri/src/sidecar/terminal.rs`
  - `spawn_pane(opts: PaneSpawnInput) -> Result<Pane>` — fork PTY, wire stdin/stdout
  - `write_to_pane(pane_id: &str, data: &[u8])` — pty stdin write
  - `resize_pane(pane_id: &str, cols, rows)` — SIGWINCH equivalent
  - `kill_pane(pane_id: &str)` — SIGTERM, then SIGKILL after 5s
- Default shell selection per platform:
  - Windows: `pwsh.exe` if available, else `powershell.exe`
  - macOS: `$SHELL` (typically `zsh`)
  - Linux: `$SHELL` (typically `bash`)
- Output streaming:
  - Spawn a tokio task per pane reading from PTY master
  - Each line emits `pane.{id}.line` event with `{ k: 'out'|'err', text, seq }`
  - Status detection: regex on last 5 lines (per NEURON_TERMINAL_REPORT § state machine)
- Ring buffer (5,000 lines per pane) in memory. On exceed, oldest 1,000 dropped. On pane close, last 5,000 lines persisted to `pane_lines`.
- Pane status state machine:
  - `idle` → `starting` (process spawning) → `running` (process active) → `awaiting_approval` (regex match) → `running` (continuing) → `success` (exit 0) | `error` (exit non-zero)
- Cleanup hook on app shutdown: kill all child PTYs (no orphans on next launch)

## Replace the stubs in WP-W2-03

- `terminal:spawn(opts)` → real PTY spawn, returns Pane row from DB
- `terminal:list` → unchanged (DB read)
- `terminal:kill(id)` → real SIGTERM
- New commands:
  - `terminal:write(paneId, data)` → write bytes to PTY stdin
  - `terminal:resize(paneId, cols, rows)` → resize event
  - `terminal:lines(paneId, sinceSeq?)` → ring-buffer read (for hydration on UI mount)

## Acceptance criteria

- [ ] `terminal:spawn({ cwd: '~', cmd: undefined })` opens default shell, returns Pane with valid PID
- [ ] Typing `ls` and pressing enter (via `terminal:write`) produces output streamed to frontend (verify via `listen('pane.{id}.line', ...)`)
- [ ] 4 panes (2x2 layout) work simultaneously without interference
- [ ] Killing app cleans all child PTY processes (verify on Linux/macOS with `ps`, on Windows with Task Manager)
- [ ] Pane status correctly transitions idle → running → success (exit `ls` with `exit` command, status becomes `success`)
- [ ] Resize from 80x24 to 120x40 reflects in shell (`tput cols` returns 120 in shell after resize)
- [ ] Ring buffer persists last 5,000 lines on pane close → restoring with `terminal:lines(id)` works after app restart

## Verification commands

```bash
cargo test --manifest-path src-tauri/Cargo.toml -- sidecar::terminal
# manual smoke (pnpm tauri dev → devtools):
const pane = await invoke('terminal:spawn', { input: { cwd: '~' } });
await listen(`pane.${pane.id}.line`, e => console.log('LINE:', e.payload));
await invoke('terminal:write', { paneId: pane.id, data: 'echo hello\n' });
// expect "hello" line in console within 500ms
await invoke('terminal:kill', { id: pane.id });
```

## Notes / risks

- Windows ConPTY has known quirks under high stdout throughput + frequent resize. Throttle resize events to ≤10/sec.
- Long-output processes consuming RAM — ring buffer cap enforced; flush every 1k lines to DB, drop in-memory.
- Killing pane mid-write loses last bytes — accept; not safety-critical.
- Agent kind detection (claude-code / codex / gemini / shell) inferred from `opts.cmd` substring. Default = `shell`.
- ANSI escape sequences: strip CSI cursor-control before storing in DB; preserve raw bytes for live render via `pane.{id}.line` events. Frontend uses xterm.js or a simple ANSI parser to render.

## Sub-agent reminders

- Read `NEURON_TERMINAL_REPORT.md` BEFORE starting — schema for `Pane`, status state machine, regex patterns.
- Do NOT couple this WP to WP-W2-04 (LangGraph). Terminal panes are independent of agent runtime.
- Do NOT add xterm.js to frontend in this WP — UI already exists in prototype using simple line rendering. xterm.js integration is WP-W2-08.

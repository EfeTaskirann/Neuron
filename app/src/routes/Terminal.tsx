// Ports `Neuron Design/app/terminal.jsx::TerminalRoute`. Backend
// data sources: usePanes() (snapshot of every pane) +
// usePaneLines(paneId) (per-pane scrollback + live line events).
//
// Tab-strip layout: panes accumulate over time (swarm launches each
// produce 9 panes), and trying to render every xterm at once melts
// the renderer + spams live-line subscriptions for dead PTYs. The
// route now shows every pane in a horizontally-scrollable tab strip
// and mounts an xterm only for the *active* tab. Closed/error panes
// stay visible (their scrollback hydrates from the persisted
// `pane_lines` table) until the user hits "Clean closed".
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { NIcon } from '../components/icons';
import { useActiveProject } from '../hooks/useActiveProject';
import { usePanes } from '../hooks/usePanes';
import { usePaneLines } from '../hooks/usePaneLines';
import { useMailbox } from '../hooks/useMailbox';
import {
  useTerminalDelete,
  useTerminalKill,
  useTerminalPurgeClosed,
  useTerminalResize,
  useTerminalSpawn,
  useTerminalWrite,
} from '../hooks/mutations';
import type { MailboxEntry, Pane, PaneLine } from '../lib/bindings';

interface AgentInfo {
  name: string;
  accent: string;
  icon: 'claude' | 'openai' | 'gemini' | 'shell';
}

// Display metadata indexed by Pane.agent (claude/codex/gemini/
// shell). Kept inline because the prototype's `data.agents`
// lookup table was UI-only and never going to be a backend
// concern.
const AGENT_META: Record<string, AgentInfo> = {
  'claude-code': { name: 'Claude', accent: 'claude', icon: 'claude' },
  codex: { name: 'Codex', accent: 'openai', icon: 'openai' },
  gemini: { name: 'Gemini', accent: 'gemini', icon: 'gemini' },
  shell: { name: 'Shell', accent: 'shell', icon: 'shell' },
};

function metaFor(agent: string): AgentInfo {
  return AGENT_META[agent] ?? { name: agent, accent: 'shell', icon: 'shell' };
}

// Panes in a terminal state — kill alone won't free the row, only
// `terminal:purge_closed` will. UI uses this set to skip the live
// `panes:{id}:line` subscription (dead PTY emits nothing) and to
// disable the per-tab close button in favour of the bulk cleanup.
const TERMINAL_STATUSES = new Set(['closed', 'error', 'success']);

export function TerminalRoute(): JSX.Element {
  const { data: panes = [], isLoading, isError, error } = usePanes();
  const [activeId, setActiveId] = useState<string | null>(null);

  // Resolve the active pane at render time so a purged-out
  // `activeId` falls back to the first live pane without an effect
  // round-trip. The state still owns the user's last click; this
  // memo just narrows it to whatever exists right now.
  const resolvedActiveId = useMemo<string | null>(() => {
    if (panes.length === 0) return null;
    if (activeId != null && panes.some((p) => p.id === activeId)) return activeId;
    return panes[0]!.id;
  }, [panes, activeId]);

  if (isLoading) {
    return <div className="term-route route-loading">Loading panes…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (panes.length === 0) {
    return (
      <div className="term-route term-route-empty">
        <p className="text-muted">No panes yet. Spawn one to get started.</p>
        <NewPaneButton />
      </div>
    );
  }
  const activePane =
    panes.find((p) => p.id === resolvedActiveId) ?? panes[0]!;
  return (
    <div className="term-route">
      <MailboxPanel />
      <div className="term-toolbar">
        <NewPaneButton />
        <PurgeClosedButton panes={panes} />
      </div>
      <div className="pane-main">
        <PaneTabStrip
          panes={panes}
          activeId={activePane.id}
          onSelect={setActiveId}
        />
        <div className="pane-active">
          <PaneView
            key={activePane.id}
            pane={activePane}
            active
            onActivate={() => setActiveId(activePane.id)}
          />
        </div>
      </div>
      <TermStatusBar panes={panes} />
    </div>
  );
}

// Inline spawn dialog. Button collapses into a small form; submit
// calls terminal:spawn with the typed cwd. cmd/cols/rows fall
// back to the platform default per WP-W2-06's ergonomics.
//
// Default cwd is the App-level active project folder (if set);
// the user can still edit the field for one-off spawns elsewhere.
// Pre-2026-05-13 this defaulted to literal `.` (process CWD = the
// Neuron .exe install dir), which was almost never what the user
// wanted.
function NewPaneButton(): JSX.Element {
  const spawn = useTerminalSpawn();
  const { project } = useActiveProject();
  const defaultCwd = project?.path ?? '.';
  const [open, setOpen] = useState(false);
  const [cwd, setCwd] = useState(defaultCwd);

  if (!open) {
    return (
      <button className="btn primary" onClick={() => setOpen(true)}>
        <NIcon name="plus" size={14} />
        <span>New pane</span>
      </button>
    );
  }
  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (!cwd.trim()) return;
    spawn.mutate(
      {
        cwd: cwd.trim(),
        cmd: null,
        cols: null,
        rows: null,
        agentKind: null,
        role: null,
        workspace: null,
        extraEnv: null,
      },
      {
        onSuccess: () => {
          setOpen(false);
          setCwd(defaultCwd);
        },
      },
    );
  };
  return (
    <form className="new-pane-form" onSubmit={handleSubmit}>
      <input
        autoFocus
        value={cwd}
        onChange={(e) => setCwd(e.target.value)}
        placeholder="cwd (e.g. ~/work)"
        aria-label="Working directory"
      />
      <button type="submit" className="btn primary sm" disabled={spawn.isPending}>
        {spawn.isPending ? 'Spawning…' : 'Spawn'}
      </button>
      <button
        type="button"
        className="btn ghost sm"
        onClick={() => setOpen(false)}
      >
        Cancel
      </button>
    </form>
  );
}

// "Clean closed" — bulk-removes closed/error/success panes from the
// DB so the tab strip stops accumulating after each swarm launch.
// Disabled when nothing is purgeable so users don't fire a no-op.
function PurgeClosedButton({ panes }: { panes: Pane[] }): JSX.Element {
  const purge = useTerminalPurgeClosed();
  const purgeable = panes.filter((p) => TERMINAL_STATUSES.has(p.status)).length;
  const disabled = purgeable === 0 || purge.isPending;
  return (
    <button
      type="button"
      className="btn ghost sm"
      disabled={disabled}
      onClick={() => purge.mutate()}
      title={
        purgeable === 0
          ? 'No closed panes'
          : `Remove ${purgeable} closed/errored pane${purgeable === 1 ? '' : 's'}`
      }
    >
      <NIcon name="trash" size={12} />
      <span>
        {purge.isPending
          ? 'Cleaning…'
          : `Clean closed${purgeable > 0 ? ` (${purgeable})` : ''}`}
      </span>
    </button>
  );
}

// Horizontal tab strip — every pane gets a chip with status dot,
// agent name, optional role, and a kill button. Only one pane is
// mounted in the body at a time (see `PaneView`), so this scales to
// the 50+ panes a long-running session accumulates without melting
// xterm.js. Scrollable when the strip overflows.
interface PaneTabStripProps {
  panes: Pane[];
  activeId: string;
  onSelect: (id: string) => void;
}

function PaneTabStrip({ panes, activeId, onSelect }: PaneTabStripProps): JSX.Element {
  // Tab "✕" force-removes the pane (kill + DB delete in one call) so
  // the strip actually shrinks. `terminal:kill` alone only flips
  // status to `closed` and leaves the row in place — that's why the
  // pre-fix tabs felt "unclosable".
  const del = useTerminalDelete();
  return (
    <div className="pane-tabs" role="tablist">
      {panes.map((p) => {
        const meta = metaFor(p.agent);
        const isActive = p.id === activeId;
        return (
          <button
            key={p.id}
            role="tab"
            aria-selected={isActive}
            className={`pane-tab status-${p.status}${isActive ? ' active' : ''}`}
            onClick={() => onSelect(p.id)}
            title={`${meta.name} · ${p.cwd}`}
          >
            <span className={`pane-tab-dot status-${p.status}`} />
            <span className="pane-tab-name">{meta.name}</span>
            {p.role && <span className="pane-tab-role">· {p.role}</span>}
            <span
              role="button"
              tabIndex={-1}
              className="pane-tab-close"
              title="Close pane"
              aria-disabled={del.isPending}
              onClick={(e) => {
                e.stopPropagation();
                if (del.isPending) return;
                del.mutate(p.id);
              }}
            >
              <NIcon name="close" size={10} />
            </span>
          </button>
        );
      })}
    </div>
  );
}

// Cross-pane event log. Renders as a slim header strip above the
// pane grid — keeps the visual hierarchy: messages first, then the
// running panes. Empty state is hidden (no row at all) so the
// pane grid can claim the full vertical space when there's nothing
// to surface.
function MailboxPanel(): JSX.Element | null {
  const { data: entries = [] } = useMailbox();
  const [expanded, setExpanded] = useState(false);
  if (entries.length === 0) return null;
  const visible = expanded ? entries : entries.slice(0, 3);
  return (
    <div className="mailbox-panel" aria-label="Mailbox">
      <div className="mailbox-head">
        <NIcon name="activity" size={12} />
        <span className="mailbox-title">Mailbox · {entries.length}</span>
        {entries.length > 3 && (
          <button className="mailbox-toggle" onClick={() => setExpanded((v) => !v)}>
            {expanded ? 'Collapse' : 'Show all'}
          </button>
        )}
      </div>
      <ul className="mailbox-list">
        {visible.map((entry) => (
          <MailboxRow key={entry.id} entry={entry} />
        ))}
      </ul>
    </div>
  );
}

function MailboxRow({ entry }: { entry: MailboxEntry }): JSX.Element {
  return (
    <li className="mailbox-row">
      <span className="mailbox-ts">{formatRelative(entry.ts)}</span>
      <code className="mailbox-from">{entry.from}</code>
      <NIcon name="arrowR" size={10} />
      <code className="mailbox-to">{entry.to}</code>
      <span className="mailbox-type">{entry.type}</span>
      <span className="mailbox-summary">{entry.summary}</span>
    </li>
  );
}

function formatRelative(ts: number): string {
  const delta = Math.max(0, Math.floor(Date.now() / 1000) - ts);
  if (delta < 60) return `${delta}s`;
  if (delta < 3600) return `${Math.floor(delta / 60)}m`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h`;
  return `${Math.floor(delta / 86400)}d`;
}

interface PaneViewProps {
  pane: Pane;
  active: boolean;
  onActivate: () => void;
}

function PaneView({ pane, active, onActivate }: PaneViewProps): JSX.Element {
  const agent = metaFor(pane.agent);
  return (
    <div
      className={`pane status-${pane.status}${active ? ' active' : ''}`}
      onClick={onActivate}
    >
      <div className="pane-stripe" />
      <PaneHeader pane={pane} agent={agent} />
      {pane.approval && <ApprovalBanner approval={pane.approval} />}
      <PaneBody pane={pane} />
    </div>
  );
}

const STATUS_LABEL: Record<string, string> = {
  idle: 'idle',
  running: 'running',
  awaiting_approval: 'awaiting',
  success: 'done',
  error: 'error',
  starting: 'starting',
  closed: 'closed',
};

function PaneHeader({ pane, agent }: { pane: Pane; agent: AgentInfo }): JSX.Element {
  const kill = useTerminalKill();
  return (
    <div className="pane-head">
      <div className="pane-head-l">
        <AgentIcon kind={agent.icon} accent={agent.accent} />
        <span className="pane-name">{agent.name}</span>
        <span className={`pane-dot status-${pane.status}`} />
        <span className="pane-status">{STATUS_LABEL[pane.status] ?? pane.status}</span>
        {pane.role && <span className="pane-role">· {pane.role}</span>}
      </div>
      <div className="pane-cwd" title={pane.cwd}>
        {pane.cwd}
      </div>
      <div className="pane-head-r">
        <button className="icon-btn sm" title="Clear">
          <NIcon name="trash" size={12} />
        </button>
        <button className="icon-btn sm" title="Restart">
          <NIcon name="play" size={12} />
        </button>
        <button className="icon-btn sm" title="Pop out">
          <NIcon name="layers" size={12} />
        </button>
        <button
          className="icon-btn sm"
          title="Close pane"
          disabled={kill.isPending}
          onClick={(e) => {
            e.stopPropagation();
            kill.mutate(pane.id);
          }}
        >
          <NIcon name="close" size={12} />
        </button>
      </div>
    </div>
  );
}

function ApprovalBanner({ approval }: { approval: NonNullable<Pane['approval']> }): JSX.Element {
  return (
    <div className="approval-banner">
      <span className="ab-tag">tool</span>
      <code className="ab-tool">{approval.tool}</code>
      <span className="ab-arrow">→</span>
      <code className="ab-target">{approval.target}</code>
      <span className="ab-diff">
        <span className="ab-add">+{approval.added}</span>
        <span className="ab-rem">−{approval.removed}</span>
      </span>
      <div className="ab-spacer" />
      <button className="btn ghost sm">Reject</button>
      <button className="btn primary sm">Accept</button>
    </div>
  );
}

// xterm-backed pane body. Snapshot lines (terminal:lines) write
// once on mount; subsequent panes:{id}:line events stream into
// xterm directly. Keystrokes go out via terminal:write. Resize is
// hooked to a ResizeObserver around the container so layout
// switches and window resizes propagate to the PTY.
//
// Backend currently strips ANSI before emitting (see
// terminal.rs::LineEventPayload — `text` is plain). xterm still
// gives us a real cursor, scroll, font, and input handling; ANSI
// rendering follows when the backend event payload changes.
function PaneBody({ pane }: { pane: Pane }): JSX.Element {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const xtermRef = useRef<XTerm | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  // `writtenSeqsRef` guards against the snapshot/live race (a live
  // event may arrive before the snapshot resolves). `seenSnapshotLenRef`
  // is the high-water mark into the cached snapshot array — it keeps
  // the snapshot effect from re-iterating the entire scrollback on
  // every new line event (was O(n) per event, now O(new tail)).
  const writtenSeqsRef = useRef<Set<number>>(new Set());
  const seenSnapshotLenRef = useRef(0);
  // `live: false` — snapshot hydrates xterm once; the live stream is
  // owned by this component's own `panes:{id}:line` listener (below,
  // skipped for dead PTYs). Letting usePaneLines also subscribe would
  // duplicate the listener and grow an unbounded line array per pane.
  const { data: snapshot } = usePaneLines(pane.id, { live: false });
  const writeMut = useTerminalWrite();
  const resizeMut = useTerminalResize();
  const isTerminal = TERMINAL_STATUSES.has(pane.status);

  // Mount xterm once per pane. The PTY lifecycle is independent of
  // the React render — drop the instance only when the pane id
  // changes, not on every render.
  useEffect(() => {
    if (!containerRef.current) return;
    // Copy the ref's current Set into a local so the cleanup uses the
    // exact instance captured at mount time (per react-hooks rule). For
    // a `useRef<Set>` whose .current never reassigns this is equivalent,
    // but the lint rule is right to flag the pattern in general.
    const writtenSeqs = writtenSeqsRef.current;
    const term = new XTerm({
      fontFamily: 'var(--font-mono), Menlo, Consolas, monospace',
      fontSize: 12,
      theme: { background: '#0a0a0f', foreground: '#e6e6ea' },
      cursorBlink: true,
      convertEol: true,
      scrollback: 5000,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(containerRef.current);
    try {
      fit.fit();
    } catch {
      // fit() can throw if the container has 0 dimensions during
      // initial mount; ResizeObserver below picks it up shortly.
    }

    const onDataDisp = term.onData((data) => {
      writeMut.mutate({ paneId: pane.id, data });
    });

    xtermRef.current = term;
    fitRef.current = fit;
    return () => {
      onDataDisp.dispose();
      term.dispose();
      xtermRef.current = null;
      fitRef.current = null;
      writtenSeqs.clear();
      seenSnapshotLenRef.current = 0;
    };
    // pane.id is the only useful dep — write/resize mutations are
    // stable refs from TanStack Query.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pane.id]);

  // Resize observer: refit xterm when the container size changes,
  // then propagate the new cols/rows to the PTY. Window drags fire
  // ResizeObserver every animation frame; `fit()` stays inline so
  // the visual snaps immediately, but the IPC `terminal:resize` is
  // trailing-debounced so the PTY only sees the final size.
  useEffect(() => {
    if (!containerRef.current) return;
    let pendingTimer: ReturnType<typeof setTimeout> | null = null;
    let lastCols = -1;
    let lastRows = -1;
    const obs = new ResizeObserver(() => {
      const fit = fitRef.current;
      const term = xtermRef.current;
      if (!fit || !term) return;
      try {
        fit.fit();
      } catch {
        return;
      }
      // Skip the IPC entirely if the cell grid didn't actually change
      // — happens often when the wrapper resizes by a sub-cell amount.
      if (term.cols === lastCols && term.rows === lastRows) return;
      lastCols = term.cols;
      lastRows = term.rows;
      if (pendingTimer != null) clearTimeout(pendingTimer);
      // Closed/errored panes have no PTY behind them; resize would 404.
      if (isTerminal) return;
      pendingTimer = setTimeout(() => {
        pendingTimer = null;
        const t = xtermRef.current;
        if (!t) return;
        resizeMut.mutate({ paneId: pane.id, cols: t.cols, rows: t.rows });
      }, 80);
    });
    obs.observe(containerRef.current);
    return () => {
      obs.disconnect();
      if (pendingTimer != null) clearTimeout(pendingTimer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pane.id, isTerminal]);

  // Write the snapshot scrollback once it arrives, then incrementally
  // append new tail entries pushed by `usePaneLines`'s cache update.
  // The watermark avoids re-scanning the entire snapshot array (which
  // could be tens of thousands of lines) on every line event — a hot
  // path for any active terminal. `writtenSeqsRef` still gates the
  // initial-write loop so a snapshot/live race can't double-render.
  useEffect(() => {
    const term = xtermRef.current;
    if (!term || !snapshot) return;
    // Snapshot length shrank — cache was reset (refetch / pane swap);
    // restart from index 0.
    if (snapshot.length < seenSnapshotLenRef.current) {
      seenSnapshotLenRef.current = 0;
    }
    for (let i = seenSnapshotLenRef.current; i < snapshot.length; i++) {
      const line = snapshot[i]!;
      if (writtenSeqsRef.current.has(line.seq)) continue;
      writtenSeqsRef.current.add(line.seq);
      term.write(line.text + '\r\n');
    }
    seenSnapshotLenRef.current = snapshot.length;
  }, [snapshot]);

  // Live subscription — write each event payload as a single line.
  // Skipped for closed/error/success panes: the PTY is gone, so no
  // events will ever fire and the subscription is wasted work (and
  // for a long-lived session, 50 dead subscriptions was the symptom
  // the tab refactor is meant to cure).
  useEffect(() => {
    if (isTerminal) return;
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<PaneLine>(`panes:${pane.id}:line`, (event) => {
      const term = xtermRef.current;
      if (!term) return;
      const incoming = event.payload;
      if (writtenSeqsRef.current.has(incoming.seq)) return;
      writtenSeqsRef.current.add(incoming.seq);
      term.write(incoming.text + '\r\n');
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        console.warn('[PaneBody] failed to subscribe', err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [pane.id, isTerminal]);

  return <div className="pane-body pane-body-xterm" ref={containerRef} />;
}

interface TermStatusBarProps {
  panes: Pane[];
}

function TermStatusBar({ panes }: TermStatusBarProps): JSX.Element {
  const counts = useMemo(() => {
    const c = { running: 0, idle: 0, error: 0, awaiting: 0, success: 0, closed: 0 };
    for (const p of panes) {
      if (p.status === 'running') c.running++;
      else if (p.status === 'idle') c.idle++;
      else if (p.status === 'error') c.error++;
      else if (p.status === 'awaiting_approval') c.awaiting++;
      else if (p.status === 'success') c.success++;
      else if (p.status === 'closed') c.closed++;
    }
    return c;
  }, [panes]);

  return (
    <div className="term-statusbar">
      <div className="tsb-l">
        <div className="tsb-ws">
          <NIcon name="layers" size={12} /> personal · {panes.length} panes
        </div>
      </div>
      <div className="tsb-c">
        {counts.running > 0 && (
          <span className="tsb-pill st-running">
            <span className="pulse-dot" />
            {counts.running} running
          </span>
        )}
        {counts.awaiting > 0 && (
          <span className="tsb-pill st-awaiting">{counts.awaiting} awaiting</span>
        )}
        {counts.success > 0 && (
          <span className="tsb-pill st-success">{counts.success} done</span>
        )}
        {counts.idle > 0 && <span className="tsb-pill st-idle">{counts.idle} idle</span>}
        {counts.error > 0 && (
          <span className="tsb-pill st-error">{counts.error} error</span>
        )}
        {counts.closed > 0 && (
          <span className="tsb-pill st-idle">{counts.closed} closed</span>
        )}
      </div>
      <div className="tsb-r" />
    </div>
  );
}

function AgentIcon({
  kind,
  accent,
}: {
  kind: AgentInfo['icon'];
  accent: string;
}): JSX.Element {
  const c = `var(--agent-${accent})`;
  if (kind === 'claude')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="9" fill="none" stroke={c} strokeWidth="1.6" />
        <path
          d="M8 9 L12 15 L16 9"
          stroke={c}
          strokeWidth="1.6"
          fill="none"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    );
  if (kind === 'openai')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <path
          d="M12 3 L20 8 V16 L12 21 L4 16 V8 Z"
          fill="none"
          stroke={c}
          strokeWidth="1.6"
          strokeLinejoin="round"
        />
      </svg>
    );
  if (kind === 'gemini')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <path d="M12 2 L14 10 L22 12 L14 14 L12 22 L10 14 L2 12 L10 10 Z" fill={c} />
      </svg>
    );
  return (
    <svg width="14" height="14" viewBox="0 0 24 24">
      <path
        d="M5 9 L9 12 L5 15 M11 16 L17 16"
        stroke={c}
        strokeWidth="1.6"
        fill="none"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

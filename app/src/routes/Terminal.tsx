// Ports `Neuron Design/app/terminal.jsx::TerminalRoute`. Backend
// data sources: usePanes() (snapshot of every pane) +
// usePaneLines(paneId) (per-pane scrollback + live line events).
//
// xterm.js integration (WP-W2-08 spec §7) is deferred to a follow-
// up sub-commit — this route renders the structured line shape
// (`{seq,k,text}`) the backend already strips ANSI from. Users see
// real PTY lines but without colour rendering until xterm lands.
//
// Spawn / write / kill mutations live in Phase E. For now the
// route shows the empty state and the layout switcher; new panes
// require running a `terminalSpawn` command from devtools or the
// upcoming Phase E button.
import { useMemo, useState } from 'react';
import { NIcon } from '../components/icons';
import { usePanes } from '../hooks/usePanes';
import { usePaneLines } from '../hooks/usePaneLines';
import { useMailbox } from '../hooks/useMailbox';
import type { MailboxEntry, Pane, PaneLine } from '../lib/bindings';

type Layout = '1' | '2v' | '2h' | '2x2' | '3x4';

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

export function TerminalRoute(): JSX.Element {
  const { data: panes = [], isLoading, isError, error } = usePanes();
  const [layout, setLayout] = useState<Layout>('2x2');
  const [activeId, setActiveId] = useState<string | null>(null);

  if (isLoading) {
    return <div className="term-route route-loading">Loading panes…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (panes.length === 0) {
    return (
      <div className="term-route term-route-empty">
        <p className="text-muted">
          No panes yet. Spawn one from the topbar's "+ New pane" button.
        </p>
      </div>
    );
  }
  const active = activeId ?? panes[0]!.id;
  return (
    <div className="term-route">
      <MailboxPanel />
      <div className={`pane-grid layout-${layout}`}>
        {panes.map((p) => (
          <PaneView
            key={p.id}
            pane={p}
            active={p.id === active}
            onActivate={() => setActiveId(p.id)}
          />
        ))}
      </div>
      <TermStatusBar layout={layout} setLayout={setLayout} panes={panes} />
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
        <button className="icon-btn sm" title="Close">
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

function PaneBody({ pane }: { pane: Pane }): JSX.Element {
  const { data: lines = [] } = usePaneLines(pane.id);
  return (
    <div className="pane-body">
      {lines.map((ln) => (
        <TermLine key={ln.seq} line={ln} />
      ))}
      {pane.status === 'running' && (
        <div className="term-cursor-line">
          <span className="term-cursor" />
        </div>
      )}
    </div>
  );
}

function TermLine({ line }: { line: PaneLine }): JSX.Element {
  if (line.k === 'prompt') {
    return (
      <div className="tl prompt">
        <span className="tl-prompt-sigil">{line.text || '›'}</span>
      </div>
    );
  }
  if (line.k === 'command') {
    return (
      <div className="tl command">
        <span className="tl-prompt-sigil">›</span> <span className="tl-cmd">{line.text}</span>
      </div>
    );
  }
  if (line.k === 'thinking') {
    return (
      <div className="tl thinking">
        <span className="tl-think-dot" /> {line.text}
      </div>
    );
  }
  if (line.k === 'tool') {
    return (
      <div className="tl tool">
        <NIcon name="wrench" size={11} /> <span>{line.text}</span>
      </div>
    );
  }
  if (line.k === 'err') return <div className="tl err">{line.text}</div>;
  if (line.k === 'sys') return <div className="tl sys">{line.text}</div>;
  return <div className="tl out">{line.text}</div>;
}

interface TermStatusBarProps {
  layout: Layout;
  setLayout: (l: Layout) => void;
  panes: Pane[];
}

function TermStatusBar({ layout, setLayout, panes }: TermStatusBarProps): JSX.Element {
  const counts = useMemo(() => {
    const c = { running: 0, idle: 0, error: 0, awaiting: 0, success: 0 };
    for (const p of panes) {
      if (p.status === 'running') c.running++;
      else if (p.status === 'idle') c.idle++;
      else if (p.status === 'error') c.error++;
      else if (p.status === 'awaiting_approval') c.awaiting++;
      else if (p.status === 'success') c.success++;
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
      </div>
      <div className="tsb-r">
        <LayoutSwitcher layout={layout} setLayout={setLayout} />
      </div>
    </div>
  );
}

const LAYOUT_OPTS: { id: Layout; icon: JSX.Element }[] = [
  { id: '1', icon: <rect x="3" y="3" width="18" height="18" rx="2" /> },
  {
    id: '2v',
    icon: (
      <g>
        <rect x="3" y="3" width="8" height="18" rx="2" />
        <rect x="13" y="3" width="8" height="18" rx="2" />
      </g>
    ),
  },
  {
    id: '2h',
    icon: (
      <g>
        <rect x="3" y="3" width="18" height="8" rx="2" />
        <rect x="3" y="13" width="18" height="8" rx="2" />
      </g>
    ),
  },
  {
    id: '2x2',
    icon: (
      <g>
        <rect x="3" y="3" width="8" height="8" rx="2" />
        <rect x="13" y="3" width="8" height="8" rx="2" />
        <rect x="3" y="13" width="8" height="8" rx="2" />
        <rect x="13" y="13" width="8" height="8" rx="2" />
      </g>
    ),
  },
  {
    id: '3x4',
    icon: (
      <g>
        <rect x="3" y="3" width="5" height="8" rx="1.2" />
        <rect x="9.5" y="3" width="5" height="8" rx="1.2" />
        <rect x="16" y="3" width="5" height="8" rx="1.2" />
        <rect x="3" y="13" width="5" height="8" rx="1.2" />
        <rect x="9.5" y="13" width="5" height="8" rx="1.2" />
        <rect x="16" y="13" width="5" height="8" rx="1.2" />
      </g>
    ),
  },
];

function LayoutSwitcher({
  layout,
  setLayout,
}: {
  layout: Layout;
  setLayout: (l: Layout) => void;
}): JSX.Element {
  return (
    <div className="layout-switcher">
      {LAYOUT_OPTS.map((o) => (
        <button
          key={o.id}
          className={`ls-btn${layout === o.id ? ' active' : ''}`}
          onClick={() => setLayout(o.id)}
          title={o.id}
        >
          <svg
            width="16"
            height="16"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.75"
          >
            {o.icon}
          </svg>
        </button>
      ))}
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

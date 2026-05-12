// WP-W2-08 phase A — shell scaffold. Mirrors `Neuron Design/app/shell.jsx`'s
// DOM and class names verbatim so the moved CSS resolves unchanged.
// Routes are stubs ("coming soon"); phases B/C/D port real routes
// in over their respective hooks. User/workspace strings are
// placeholders until `useMe()` lands in phase B.
import { useState } from 'react';
import { Brandmark, NIcon, type IconName } from './components/icons';
import { ErrorBoundary } from './components/ErrorBoundary';
import { useMe } from './hooks/useMe';
import { useRuns } from './hooks/useRuns';
import { useRunCreate } from './hooks/mutations';
import { AgentsRoute } from './routes/AgentsRoute';
import { RunsRoute } from './routes/RunsRoute';
import { MCPRoute } from './routes/MCPRoute';
import { SettingsRoute } from './routes/SettingsRoute';
import { Canvas } from './routes/Canvas';
import { RunInspector } from './routes/RunInspector';
import { TerminalRoute } from './routes/Terminal';
import { SwarmRoute } from './routes/SwarmRoute';
import { TerminalSwarmRoute } from './routes/TerminalSwarm';

type Route =
  | 'canvas'
  | 'terminal'
  | 'terminal-swarm'
  | 'swarm'
  | 'agents'
  | 'runs'
  | 'mcp'
  | 'settings';

const NAV: { id: Route; label: string; icon: IconName }[] = [
  { id: 'canvas', label: 'Workflow', icon: 'workflow' },
  { id: 'terminal', label: 'Terminal', icon: 'server' },
  { id: 'terminal-swarm', label: 'Terminal Swarm', icon: 'bot' },
  { id: 'swarm', label: 'Swarm', icon: 'bot' },
  { id: 'agents', label: 'Agents', icon: 'bot' },
  { id: 'runs', label: 'Runs', icon: 'activity' },
  { id: 'mcp', label: 'MCP', icon: 'store' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
];

const TOPBAR_TITLE: Record<Route, string> = {
  canvas: 'Daily summary',
  terminal: 'Terminal',
  'terminal-swarm': 'Terminal Swarm',
  swarm: 'Swarm',
  agents: 'Agents',
  runs: 'Runs',
  mcp: 'MCP marketplace',
  settings: 'Settings',
};

interface SidebarProps {
  route: Route;
  onNavigate: (next: Route) => void;
  collapsed: boolean;
  onToggle: () => void;
}

function Sidebar({ route, onNavigate, collapsed, onToggle }: SidebarProps): JSX.Element {
  const { data: me } = useMe();
  // Loading placeholders — kept terse so the layout doesn't shift
  // when the hook resolves. Workspace count comes from the backend
  // (`SELECT COUNT(*) FROM workflows`); no client-side derivation.
  const initials = me?.user.initials ?? '··';
  const userName = me?.user.name ?? 'Loading…';
  const workspaceName = me?.workspace.name ?? 'Loading…';
  const workspaceCount = me?.workspace.count;
  return (
    <nav className="sidebar" role="navigation">
      <div className="sb-brand" onClick={onToggle} role="button" title="Toggle sidebar">
        <Brandmark size={28} />
        {!collapsed && <span className="sb-wordmark">Neuron</span>}
      </div>

      {!collapsed && (
        <div className="sb-workspace">
          <div className="sb-ws-avatar">{initials}</div>
          <div className="sb-ws-meta">
            <div className="sb-ws-name">{workspaceName}</div>
            <div className="sb-ws-sub">{workspaceCount ?? '—'} workflows</div>
          </div>
          <NIcon name="chevron" size={14} style={{ opacity: 0.5 }} />
        </div>
      )}

      <ul className="sb-nav">
        {NAV.map((item) => (
          <li
            key={item.id}
            className={`sb-item${route === item.id ? ' active' : ''}`}
            onClick={() => onNavigate(item.id)}
          >
            <NIcon name={item.icon} size={18} />
            {!collapsed && <span>{item.label}</span>}
            {!collapsed && route === item.id && <span className="sb-dot" />}
          </li>
        ))}
      </ul>

      <div className="sb-foot">
        <div className="sb-foot-avatar">{initials}</div>
        {!collapsed && (
          <>
            <div className="sb-foot-meta">
              <div className="sb-foot-name">{userName}</div>
              <div className="sb-foot-sub">Free plan</div>
            </div>
            <NIcon name="chevron" size={14} style={{ opacity: 0.5 }} />
          </>
        )}
      </div>
    </nav>
  );
}

function Topbar({ route }: { route: Route }): JSX.Element {
  const runCreate = useRunCreate();
  return (
    <header className="topbar">
      <div className="topbar-l">
        <div className="topbar-crumb">
          <span className="crumb-mute">Personal</span>
          <NIcon name="chevronR" size={12} style={{ opacity: 0.4 }} />
          <span>{TOPBAR_TITLE[route]}</span>
        </div>
      </div>

      <div className="topbar-search">
        <NIcon name="search" size={14} />
        <input placeholder="Search workflows, agents, servers…" />
        <kbd>⌘K</kbd>
      </div>

      <div className="topbar-r">
        {route === 'canvas' ? (
          <button
            className="btn primary"
            disabled={runCreate.isPending}
            onClick={() => runCreate.mutate('daily-summary')}
          >
            <NIcon name="play" size={12} />
            <span>{runCreate.isPending ? 'Starting…' : 'Run'}</span>
          </button>
        ) : (
          <button className="btn primary">
            <NIcon name="plus" size={14} />
            <span>New</span>
          </button>
        )}
      </div>
    </header>
  );
}

function RouteStub({ route }: { route: Route }): JSX.Element {
  return (
    <div className="route-stub" data-testid={`route-stub-${route}`}>
      <h2>{TOPBAR_TITLE[route]}</h2>
      <p>{route} — coming soon (WP-W2-08 phase B/C/D)</p>
    </div>
  );
}

// Routes are wrapped in <ErrorBoundary> per ADR-0005: a query
// failure in any route surfaces a recoverable retry card instead
// of crashing the entire shell.
function RouteHost({ route }: { route: Route }): JSX.Element {
  switch (route) {
    case 'canvas':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load workflow">
          <Canvas />
        </ErrorBoundary>
      );
    case 'terminal':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load terminal">
          <TerminalRoute />
        </ErrorBoundary>
      );
    case 'terminal-swarm':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load terminal swarm">
          <TerminalSwarmRoute />
        </ErrorBoundary>
      );
    case 'swarm':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load swarm">
          <SwarmRoute />
        </ErrorBoundary>
      );
    case 'agents':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load agents">
          <AgentsRoute />
        </ErrorBoundary>
      );
    case 'runs':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load runs">
          <RunsRoute />
        </ErrorBoundary>
      );
    case 'mcp':
      return (
        <ErrorBoundary fallbackTitle="Couldn't load servers">
          <MCPRoute />
        </ErrorBoundary>
      );
    case 'settings':
      // SettingsRoute has no data deps, but ErrorBoundary still
      // catches render-time bugs while we iterate on the panes.
      return (
        <ErrorBoundary fallbackTitle="Settings unavailable">
          <SettingsRoute />
        </ErrorBoundary>
      );
    default:
      return <RouteStub route={route} />;
  }
}

export function App(): JSX.Element {
  const [route, setRoute] = useState<Route>('canvas');
  const [collapsed, setCollapsed] = useState(false);
  const [showInspector, setShowInspector] = useState(true);
  // Pick the most recent run as the inspector's default subject.
  // `runs:list` is already sorted started_at DESC server-side.
  const { data: runs = [] } = useRuns();
  const inspectorRunId = runs[0]?.id ?? null;
  const inspectorOpen = route === 'canvas' && showInspector;
  return (
    <div
      className={`app-shell${collapsed ? ' collapsed' : ''}${
        inspectorOpen ? ' has-inspector' : ''
      }`}
    >
      <Sidebar
        route={route}
        onNavigate={setRoute}
        collapsed={collapsed}
        onToggle={() => setCollapsed((c) => !c)}
      />
      <Topbar route={route} />
      <main className="app-main">
        <RouteHost route={route} />
      </main>
      {inspectorOpen && (
        <aside className="app-inspector">
          <ErrorBoundary fallbackTitle="Couldn't load run">
            <RunInspector runId={inspectorRunId} onClose={() => setShowInspector(false)} />
          </ErrorBoundary>
        </aside>
      )}
    </div>
  );
}

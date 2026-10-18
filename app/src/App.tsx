// WP-W2-08 — app shell. Mirrors `Neuron Design/app/shell.jsx`'s DOM
// and class names verbatim so the moved CSS resolves unchanged. Every
// route is wired to a real component in `RouteHost`; the older
// surfaces are developer features (see `NAV`). The shell itself is
// gated behind `ProjectPickerRoute` until an active project is picked
// (folder = cwd for Terminal / Terminal Swarm spawns). User/workspace
// strings come from `useMe()`.
import { Fragment, useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { ask, open } from '@tauri-apps/plugin-dialog';
import { confirmWarning } from './lib/confirm';
import { Brandmark, NIcon, type IconName } from './components/icons';
import { ErrorBoundary } from './components/ErrorBoundary';
import { useMe } from './hooks/useMe';
import { useRuns } from './hooks/useRuns';
import { useRunCreate } from './hooks/mutations';
import { useActiveProject } from './hooks/useActiveProject';
import { useSwarmTermAlerts, useSwarmTermSessionSnapshot } from './hooks/useTerminalSwarmSession';
import { useAppFeatures } from './hooks/useAppFeatures';
import { ProjectPickerRoute } from './routes/ProjectPickerRoute';
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

interface NavItem {
  id: Route;
  label: string;
  icon: IconName;
  /** Developer feature (`NEURON_DEV_FEATURES=1`): listed only when on. */
  dev?: boolean;
}

// Terminal Swarm first: it is the flagship and the default route. The
// developer group comes last, under its own label.
const NAV: NavItem[] = [
  { id: 'terminal-swarm', label: 'Terminal Swarm', icon: 'bot' },
  { id: 'terminal', label: 'Terminal', icon: 'server' },
  { id: 'mcp', label: 'MCP', icon: 'store' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
  { id: 'canvas', label: 'Workflow', icon: 'workflow', dev: true },
  { id: 'swarm', label: 'Swarm', icon: 'bot', dev: true },
  { id: 'agents', label: 'Agents', icon: 'bot', dev: true },
  { id: 'runs', label: 'Runs', icon: 'activity', dev: true },
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
  /** Show the developer routes (`useAppFeatures().devFeatures`). */
  devFeatures: boolean;
}

function Sidebar({
  route,
  onNavigate,
  collapsed,
  onToggle,
  devFeatures,
}: SidebarProps): JSX.Element {
  const { data: me } = useMe();
  // Loading placeholders — kept terse so the layout doesn't shift
  // when the hook resolves.
  const initials = me?.user.initials ?? '··';
  const userName = me?.user.name ?? 'Loading…';
  const items = NAV.filter((item) => devFeatures || !item.dev);
  return (
    <nav className="sidebar" role="navigation" aria-label="Main">
      <div
        className="sb-brand"
        onClick={onToggle}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onToggle();
          }
        }}
        role="button"
        tabIndex={0}
        title="Toggle sidebar"
      >
        <Brandmark size={28} />
        {!collapsed && <span className="sb-wordmark">Neuron</span>}
      </div>

      <ul className="sb-nav">
        {items.map((item, i) => (
          <Fragment key={item.id}>
            {item.dev && !items[i - 1]?.dev && (
              <li className="sb-section" role="separator" aria-label="Developer">
                {collapsed ? null : 'Developer'}
              </li>
            )}
            <li
              className={`sb-item${route === item.id ? ' active' : ''}`}
              role="button"
              tabIndex={0}
              aria-current={route === item.id ? 'page' : undefined}
              onClick={() => onNavigate(item.id)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  onNavigate(item.id);
                }
              }}
            >
              <NIcon name={item.icon} size={18} />
              {!collapsed && <span>{item.label}</span>}
              {!collapsed && route === item.id && <span className="sb-dot" />}
            </li>
          </Fragment>
        ))}
      </ul>

      {/* The local user's name (Settings → Account); informational, not a menu. */}
      <div className="sb-foot">
        <div className="sb-foot-avatar">{initials}</div>
        {!collapsed && (
          <div className="sb-foot-meta">
            <div className="sb-foot-name">{userName}</div>
          </div>
        )}
      </div>
    </nav>
  );
}

/**
 * While the swarm runs, closing the window stops every agent — ask first.
 * (A JS close listener makes the window wait for it, then closes it
 * itself unless prevented: `core:window:allow-destroy`.)
 */
function useConfirmCloseWhileSwarmRuns(running: boolean): void {
  const runningRef = useRef(running);
  useEffect(() => {
    runningRef.current = running;
  }, [running]);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    try {
      getCurrentWindow()
        .onCloseRequested(async (event) => {
          if (!runningRef.current) return;
          let close = true;
          try {
            close = await ask(
              'Swarm çalışıyor. Pencere kapanırsa bütün ajanlar durur ve yarım kalan işleri ' +
                'kaybolur. Kapatılsın mı?',
              { title: 'Neuron’u kapat', kind: 'warning', okLabel: 'Kapat', cancelLabel: 'Vazgeç' },
            );
          } catch {
            // The dialog failed: close as asked rather than trap the user.
          }
          if (!close) event.preventDefault();
        })
        .then((fn) => {
          if (cancelled) fn();
          else unlisten = fn;
        })
        .catch(() => {});
    } catch {
      // No Tauri window (tests, a plain browser).
    }
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}

function Topbar({
  route,
  swarmProjectDir,
}: {
  route: Route;
  /** The running swarm's folder, or `null`. */
  swarmProjectDir: string | null;
}): JSX.Element {
  const runCreate = useRunCreate();
  const { project, setProject } = useActiveProject();
  // "Change" opens the native folder picker directly instead of clearing
  // the active project and dropping the user onto the full-screen
  // ProjectPickerRoute gate. Cancelling the OS dialog (returns null) is
  // a no-op, so the current project is preserved — the user can back out
  // of a project switch without ending up projectless and stuck.
  const changeProject = async () => {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected !== 'string' || selected.length === 0) return;
      if (
        swarmProjectDir != null &&
        selected !== swarmProjectDir &&
        !(await confirmWarning({
          title: 'Projeyi değiştir',
          okLabel: 'Değiştir',
          message:
            `Swarm şu an şu klasörde çalışıyor:\n${swarmProjectDir}\n\nProje değişince swarm ` +
            'orada çalışmayı sürdürür; yeni projede çalıştırmak için onu durdurup orada başlatman ' +
            'gerekir. Proje değiştirilsin mi?',
        }))
      ) {
        return;
      }
      setProject(selected);
    } catch {
      // Dialog plugin errors are non-fatal — keep the current project.
    }
  };
  return (
    <header className="topbar">
      <div className="topbar-l">
        <div className="topbar-crumb">
          <span>{TOPBAR_TITLE[route]}</span>
        </div>
        {project && (
          <div className="topbar-project-chip" title={project.path}>
            <span className="topbar-project-name">{project.name}</span>
            <button
              type="button"
              className="topbar-project-change"
              onClick={changeProject}
              aria-label="Change project"
            >
              Change
            </button>
          </div>
        )}
      </div>

      <div className="topbar-r">
        {/* The topbar primary action is "Run" on the run-oriented routes.
            Other routes carry their own primary affordance in the route
            body (Terminal's New-pane form, Agents' New-agent card, MCP
            install, Swarm chat …), so the topbar omits a redundant — and
            previously dead — button rather than firing a vague action. */}
        {route === 'canvas' || route === 'runs' ? (
          <button
            className="btn primary"
            disabled={runCreate.isPending}
            onClick={() => runCreate.mutate('daily-summary')}
          >
            <NIcon name="play" size={12} />
            <span>{runCreate.isPending ? 'Starting…' : 'Run'}</span>
          </button>
        ) : null}
      </div>
    </header>
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
          <Canvas workflowId={null} />
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
    default: {
      // Exhaustiveness backstop: the `Route` union is closed, so this
      // arm is unreachable — adding a new route without a case above
      // is a compile error here, not a blank screen.
      const unreachable: never = route;
      throw new Error(`unhandled route: ${String(unreachable)}`);
    }
  }
}

export function App(): JSX.Element {
  const [route, setRoute] = useState<Route>('terminal-swarm');
  const [collapsed, setCollapsed] = useState(false);
  const { data: features } = useAppFeatures();
  const devFeatures = features?.devFeatures ?? false;
  const [showInspector, setShowInspector] = useState(true);
  const { project } = useActiveProject();
  const { data: swarmSession } = useSwarmTermSessionSnapshot();
  // App-wide: limit / budget / exit alerts fire whatever tab is open, and
  // the shared session-status query stays fresh for the guards below.
  useSwarmTermAlerts();
  useConfirmCloseWhileSwarmRuns(swarmSession != null);
  const inspectorOpen = route === 'canvas' && showInspector;
  // Pick the most recent run as the inspector's default subject —
  // fetched only while the (developer) inspector is open. `runs:list`
  // is already sorted started_at DESC server-side.
  const { data: runs = [] } = useRuns(undefined, { enabled: inspectorOpen });
  const inspectorRunId = runs[0]?.id ?? null;
  // Project gate: until a folder is picked there is no cwd for the
  // terminal routes to spawn into, so the shell stays hidden. Picking
  // writes the store; `useSyncExternalStore` re-renders us out of the
  // gate one tick later.
  if (!project) {
    return <ProjectPickerRoute />;
  }
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
        devFeatures={devFeatures}
      />
      <Topbar route={route} swarmProjectDir={swarmSession?.projectDir ?? null} />
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

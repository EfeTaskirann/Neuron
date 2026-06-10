import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import { SwarmPane } from '../components/SwarmPane';
import { HierarchyDiagram } from '../components/SwarmHierarchy';
import { useActiveProject } from '../hooks/useActiveProject';
import { useTerminalWrite } from '../hooks/mutations';
import {
  useAutonomousMode,
  useRunClaudeUpdate,
  useStartSwarmTermSession,
  useStopSwarmTermSession,
  useSwarmTermPersonas,
  useSwarmTermSessionStatus,
} from '../hooks/useTerminalSwarmSession';
import { useClaudeUpdateProgress } from '../hooks/useClaudeUpdateProgress';
import {
  useActiveEdge,
  useAgentLifecycle,
  useRoutingEvents,
  type AgentLifecycle,
} from '../hooks/useRoutingEvents';

const SLOT_ORDER = [
  'orchestrator',
  'coordinator',
  'scout',
  'planner',
  'backend-builder',
  'frontend-builder',
  'backend-reviewer',
  'frontend-reviewer',
  'integration-tester',
];

// session_id from the backend is `swarm-term-<26-char ULID>`. A ULID's
// first 10 chars are a 48-bit ms-since-epoch timestamp in Crockford
// Base32 — we decode that to derive launch time without a separate
// backend field.
const ULID_BASE32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const ULID_RE = /^swarm-term-([0-9A-HJKMNP-TV-Z]{26})$/i;

function parseSwarmTermStartMs(sessionId: string): number | null {
  const match = ULID_RE.exec(sessionId);
  if (!match) return null;
  const tsChars = match[1].slice(0, 10).toUpperCase();
  let n = 0;
  for (const ch of tsChars) {
    const v = ULID_BASE32.indexOf(ch);
    if (v < 0) return null;
    n = n * 32 + v;
  }
  return n;
}

function formatMmSs(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

const LIFECYCLE_LABEL: Record<AgentLifecycle, string> = {
  idle: 'idle',
  assigned: 'assigned',
  building: 'building',
  review: 'review',
  done: 'done',
};

export function TerminalSwarmRoute(): JSX.Element {
  // Project comes from the App-level gate — the route is never
  // rendered with `project == null` because <ProjectPickerRoute />
  // takes over the whole window in that case. We still read defensively
  // from the hook so a future deep-link / forced-route can't crash
  // here.
  const { project } = useActiveProject();
  const projectDir = project?.path ?? null;
  const [launchError, setLaunchError] = useState<string | null>(null);
  const [chatInput, setChatInput] = useState('');
  // Click-to-expand: when an agent's pane is "expanded" the grid
  // hides the other 8 tiles and gives this one the full container.
  // ESC restores the grid. PTY width stays at 400 cols regardless,
  // so marker integrity is unaffected — only xterm.js's visual
  // viewport changes. Plan §D.
  const [expandedAgent, setExpandedAgent] = useState<string | null>(null);
  // Roving tabindex: only one pane wrapper is in the tab order at a
  // time (`tabIndex={0}`); arrow keys move the focus to a neighbour
  // and update this anchor. Mouse focus also updates the anchor so
  // Shift+Tab away → Tab back lands where the user last was.
  const [focusedAgent, setFocusedAgent] = useState<string>(SLOT_ORDER[0]!);
  const paneRefs = useRef<Map<string, HTMLDivElement>>(new Map());
  const [autonomous, setAutonomous] = useAutonomousMode();
  const { events: routeEvents } = useRoutingEvents(500);
  const activeEdge = useActiveEdge(routeEvents, 3_000);
  const lifecycle = useAgentLifecycle(routeEvents);

  // Global ESC handler — collapses the expanded pane. Only attached
  // while a pane IS expanded so we don't shadow editor / chat input
  // ESC handling in the rest of the app.
  useEffect(() => {
    if (!expandedAgent) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        setExpandedAgent(null);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [expandedAgent]);
  const { data: personas = [] } = useSwarmTermPersonas();
  const {
    data: session,
    isLoading: sessionLoading,
    isError: sessionProbeFailed,
    error: sessionProbeError,
  } = useSwarmTermSessionStatus();
  const startMut = useStartSwarmTermSession();
  const stopMut = useStopSwarmTermSession();
  const updateMut = useRunClaudeUpdate();
  const updateProgress = useClaudeUpdateProgress(updateMut.isPending);
  const writeMut = useTerminalWrite();

  const sessionActive = session != null;
  // `!sessionLoading`: while the status probe is in flight we don't
  // yet know whether a session exists — launching then would only
  // bounce off the backend's Conflict guard.
  const canLaunch =
    projectDir != null &&
    !sessionActive &&
    !sessionLoading &&
    !startMut.isPending;

  const launch = async () => {
    if (!projectDir) return;
    setLaunchError(null);
    try {
      await startMut.mutateAsync(projectDir);
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const stop = async () => {
    try {
      await stopMut.mutateAsync();
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const restart = async () => {
    if (!sessionActive) return;
    const dir = session.projectDir;
    setLaunchError(null);
    try {
      await stopMut.mutateAsync();
      await startMut.mutateAsync(dir);
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const personasById = useMemo(
    () => new Map(personas.map((p) => [p.id, p])),
    [personas],
  );
  const panesByAgent = useMemo(
    () => new Map(session?.panes.map((p) => [p.agentId, p.paneId]) ?? []),
    [session],
  );
  const orchestratorPaneId = panesByAgent.get('orchestrator') ?? null;

  // Uptime ticking lives in <SessionTimer> (below) so its 1 s interval
  // re-renders ~20px of text instead of this whole route — which would
  // otherwise reconcile the 3×3 grid and all 9 SwarmPanes every second.
  const sessionStartMs = session
    ? parseSwarmTermStartMs(session.sessionId)
    : null;

  const sendToOrchestrator = (e: FormEvent) => {
    e.preventDefault();
    const msg = chatInput.trim();
    if (!msg || !orchestratorPaneId) return;
    writeMut.mutate({
      paneId: orchestratorPaneId,
      data: `${msg}\r`,
    });
    setChatInput('');
  };

  // ── Pane keyboard navigation ────────────────────────────────────
  // Arrow keys move focus across the 3×3 grid; Home/End jump to the
  // first/last slot. Keeping the math here (rather than in CSS) lets
  // the navigation respect the actual SLOT_ORDER even if a future
  // layout swap re-orders tiles visually.
  const moveFocus = useCallback(
    (current: string, dx: number, dy: number) => {
      const idx = SLOT_ORDER.indexOf(current);
      if (idx < 0) return;
      const cols = 3;
      const row = Math.floor(idx / cols);
      const col = idx % cols;
      const nextRow = Math.min(2, Math.max(0, row + dy));
      const nextCol = Math.min(2, Math.max(0, col + dx));
      const nextIdx = nextRow * cols + nextCol;
      const nextAgent = SLOT_ORDER[nextIdx];
      if (!nextAgent || nextAgent === current) return;
      setFocusedAgent(nextAgent);
      paneRefs.current.get(nextAgent)?.focus();
    },
    [],
  );

  const handlePaneKeyDown = useCallback(
    (e: ReactKeyboardEvent<HTMLDivElement>, agentId: string) => {
      switch (e.key) {
        case 'ArrowRight':
          e.preventDefault();
          moveFocus(agentId, 1, 0);
          break;
        case 'ArrowLeft':
          e.preventDefault();
          moveFocus(agentId, -1, 0);
          break;
        case 'ArrowDown':
          e.preventDefault();
          moveFocus(agentId, 0, 1);
          break;
        case 'ArrowUp':
          e.preventDefault();
          moveFocus(agentId, 0, -1);
          break;
        case 'Home':
          e.preventDefault();
          setFocusedAgent(SLOT_ORDER[0]!);
          paneRefs.current.get(SLOT_ORDER[0]!)?.focus();
          break;
        case 'End': {
          e.preventDefault();
          const last = SLOT_ORDER[SLOT_ORDER.length - 1]!;
          setFocusedAgent(last);
          paneRefs.current.get(last)?.focus();
          break;
        }
        case 'Enter':
        case ' ': {
          // Space / Enter on the focused pane wrapper expands it —
          // mirrors the click target on the expand button without
          // forcing keyboard users to tab into it.
          if (e.target === e.currentTarget && panesByAgent.has(agentId)) {
            e.preventDefault();
            setExpandedAgent((prev) => (prev === agentId ? null : agentId));
          }
          break;
        }
        default:
          break;
      }
    },
    [moveFocus, panesByAgent],
  );

  return (
    <div className="route route-swarm-term">
      <div className="swarm-term-toolbar">
        {/*
          The inline ProjectPicker is gone — the project is established
          globally at app launch via <ProjectPickerRoute /> and shown in
          the topbar chip. The toolbar now displays the path read-only
          and the lifecycle buttons (Launch / Restart / Stop).
        */}
        <div className="swarm-term-project-readout" title={projectDir ?? ''}>
          <span className="swarm-term-project-label">Project:</span>
          <span className="swarm-term-project-path">
            {sessionActive ? session.projectDir : projectDir ?? '(none)'}
          </span>
        </div>
        {!sessionActive ? (
          <button
            type="button"
            className="btn primary sm"
            disabled={!canLaunch}
            onClick={launch}
            title="Launch the 9-agent swarm"
          >
            {startMut.isPending ? 'Launching…' : 'Launch swarm'}
          </button>
        ) : (
          <>
            <button
              type="button"
              className="btn ghost sm"
              onClick={restart}
              disabled={startMut.isPending || stopMut.isPending}
              title="Stop and re-launch with the same project"
            >
              {startMut.isPending || stopMut.isPending
                ? 'Restarting…'
                : 'Restart'}
            </button>
            <button
              type="button"
              className="btn ghost sm"
              onClick={stop}
              disabled={stopMut.isPending}
            >
              {stopMut.isPending ? 'Stopping…' : 'Stop swarm'}
            </button>
          </>
        )}
        {sessionStartMs != null && <SessionTimer startMs={sessionStartMs} />}
      </div>

      {launchError && (
        <div className="swarm-term-error" role="alert">
          {launchError}
        </div>
      )}
      {sessionProbeFailed && (
        <div className="swarm-term-error" role="alert">
          Session status unavailable:{' '}
          {sessionProbeError instanceof Error
            ? sessionProbeError.message
            : String(sessionProbeError)}
        </div>
      )}

      <HierarchyDiagram
        lifecycle={lifecycle}
        activeSource={activeEdge?.source ?? null}
        activeTarget={activeEdge?.target ?? null}
        personasById={personasById}
        panesByAgent={panesByAgent}
      />

      {sessionActive && orchestratorPaneId && (
        <form
          className={`swarm-term-chat${autonomous ? ' swarm-term-chat--auto' : ''}`}
          onSubmit={sendToOrchestrator}
        >
          <label
            className="swarm-term-auto-toggle"
            title="When ON the swarm runs end-to-end without per-step approval prompts."
          >
            <input
              type="checkbox"
              checked={autonomous}
              onChange={(e) => setAutonomous(e.target.checked)}
              aria-label="Run autonomously — suppress approval prompts"
            />
            <span aria-hidden="true" className="swarm-term-auto-toggle-track">
              <span className="swarm-term-auto-toggle-thumb" />
            </span>
            <span className="swarm-term-auto-toggle-label">
              Run autonomously
            </span>
          </label>
          {autonomous && (
            <span
              className="swarm-term-auto-chip"
              role="status"
              aria-label="Autonomous mode active"
            >
              AUTO
            </span>
          )}
          <input
            type="text"
            placeholder="Talk to @orchestrator — type your task and hit Enter"
            value={chatInput}
            onChange={(e) => setChatInput(e.target.value)}
            aria-label="Message to orchestrator"
          />
          <button
            type="submit"
            className="btn primary sm"
            disabled={!chatInput.trim()}
          >
            Send
          </button>
        </form>
      )}

      <div
        className={`swarm-term-grid${
          expandedAgent ? ' swarm-term-grid--expanded' : ''
        }`}
        role="grid"
        aria-label="Agent terminal grid — use arrow keys to navigate"
      >
        {SLOT_ORDER.map((agentId) => {
          const persona = personasById.get(agentId);
          const paneId = panesByAgent.get(agentId);
          const isExpanded = expandedAgent === agentId;
          const phase: AgentLifecycle = lifecycle[agentId] ?? 'idle';
          const isActive =
            activeEdge != null &&
            (activeEdge.source === agentId || activeEdge.target === agentId);
          const isTabStop = agentId === focusedAgent;
          const roleLabel = persona?.role ?? agentId;
          return (
            <div
              key={agentId}
              ref={(el) => {
                if (el) paneRefs.current.set(agentId, el);
                else paneRefs.current.delete(agentId);
              }}
              role="gridcell"
              tabIndex={isTabStop ? 0 : -1}
              aria-label={`Agent terminal: ${roleLabel} (${agentId}) — status ${phase}`}
              aria-current={isActive ? 'true' : undefined}
              onKeyDown={(e) => handlePaneKeyDown(e, agentId)}
              onFocus={() => setFocusedAgent(agentId)}
              className={`swarm-term-pane${
                paneId ? '' : ' swarm-term-pane-empty'
              }${isExpanded ? ' expanded' : ''}${
                isActive ? ' swarm-term-pane--active' : ''
              } swarm-term-pane--phase-${phase}`}
              data-agent-id={agentId}
            >
              <div className="swarm-term-pane-head">
                <span className="swarm-term-pane-id">@{agentId}</span>
                <span className="swarm-term-pane-role">
                  {persona?.role ?? '—'}
                </span>
                <LifecyclePill phase={phase} agentId={agentId} />
                {paneId && (
                  <button
                    type="button"
                    className="swarm-term-pane-expand"
                    title={isExpanded ? 'Restore grid (Esc)' : 'Expand pane'}
                    onClick={() =>
                      setExpandedAgent(isExpanded ? null : agentId)
                    }
                    aria-label={
                      isExpanded
                        ? `Restore grid layout from ${roleLabel}`
                        : `Expand ${roleLabel} pane`
                    }
                    aria-pressed={isExpanded}
                  >
                    {isExpanded ? '⤓' : '⛶'}
                  </button>
                )}
              </div>
              <div className="swarm-term-pane-body">
                {paneId ? (
                  <SwarmPane paneId={paneId} agentId={agentId} />
                ) : (
                  <em>idle — pick a project and launch</em>
                )}
              </div>
            </div>
          );
        })}
      </div>

      <div className="swarm-term-update-corner">
        <button
          type="button"
          className="btn ghost sm"
          disabled={sessionActive || updateMut.isPending}
          onClick={() => updateMut.mutate()}
          title={
            sessionActive
              ? "Önce session'ı kapat (Stop)"
              : updateMut.isPending
                ? 'Güncelleme sürüyor…'
                : undefined
          }
        >
          {updateMut.isPending ? 'Updating…' : 'Update Claude'}
        </button>
        {updateMut.isPending && updateProgress.lastLine && (
          <div
            className="swarm-term-update-progress"
            title={updateProgress.lines.join('\n')}
          >
            {updateProgress.lastLine}
          </div>
        )}
        {updateMut.data && !updateMut.isPending && (
          <span
            className={`swarm-term-update-status${
              updateMut.data.exitCode === 0
                ? ' swarm-term-update-status--ok'
                : ' swarm-term-update-status--err'
            }`}
            title={
              updateMut.data.exitCode === 0
                ? updateMut.data.stdoutTail.slice(-400) || 'updated'
                : updateMut.data.stderrTail.slice(-400) ||
                  `exit ${updateMut.data.exitCode}`
            }
          >
            {updateMut.data.exitCode === 0 ? '✓ updated' : '✗ failed'}
          </span>
        )}
        {updateMut.error && (
          <span
            className="swarm-term-update-status swarm-term-update-status--err"
            title={
              updateMut.error instanceof Error
                ? updateMut.error.message
                : String(updateMut.error)
            }
          >
            ✗ {updateMut.error instanceof Error
              ? updateMut.error.message.slice(0, 80)
              : 'failed'}
          </span>
        )}
      </div>
    </div>
  );
}

// ── Subcomponents ──────────────────────────────────────────────────

// Self-contained uptime clock. Owns its own `now` state + 1 s interval
// so the per-second tick only re-renders this span — not the parent
// route's 3×3 pane grid. Mounts only while a session is active (parent
// gates on `sessionStartMs != null`), so the interval is torn down on
// stop without a guard here.
function SessionTimer({ startMs }: { startMs: number }): JSX.Element {
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const label = formatMmSs(nowMs - startMs);
  return (
    <span
      className="swarm-term-session-timer"
      title="Session uptime (mm:ss)"
      aria-label={`Session uptime ${label}`}
    >
      {label}
    </span>
  );
}

interface LifecyclePillProps {
  phase: AgentLifecycle;
  agentId: string;
}

function LifecyclePill({ phase, agentId }: LifecyclePillProps): JSX.Element {
  return (
    <span
      className={`swarm-term-pane-phase swarm-term-pane-phase--${phase}`}
      role="status"
      aria-label={`Lifecycle phase for ${agentId}: ${LIFECYCLE_LABEL[phase]}`}
    >
      {LIFECYCLE_LABEL[phase]}
    </span>
  );
}

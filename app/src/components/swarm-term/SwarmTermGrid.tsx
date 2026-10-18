import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import { SwarmPane } from '../SwarmPane';
import type {
  ActiveEdge,
  AgentLifecycle,
} from '../../hooks/useAgentLifecycle';
import type { SwarmTermPersona } from '../../lib/bindings';
import { LifecyclePill } from './LifecyclePill';
import { SLOT_ORDER } from './constants';

interface SwarmTermGridProps {
  personasById: Map<string, SwarmTermPersona>;
  panesByAgent: Map<string, string>;
  lifecycle: Record<string, AgentLifecycle>;
  activeEdge: ActiveEdge | null;
}

// The 3×3 agent terminal grid. Owns the pane focus/expand interaction
// state (roving tabindex, click-to-expand, ESC-to-restore, arrow-key
// navigation) because none of it is needed by the parent route — it's
// purely local to how the grid is driven from the keyboard/mouse.
export function SwarmTermGrid({
  personasById,
  panesByAgent,
  lifecycle,
  activeEdge,
}: SwarmTermGridProps): JSX.Element {
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

  // ── Pane keyboard navigation ────────────────────────────────────
  // Arrow keys move focus across the 3×3 grid; Home/End jump to the
  // first/last slot. Keeping the math here (rather than in CSS) lets
  // the navigation respect the actual SLOT_ORDER even if a future
  // layout swap re-orders tiles visually.
  const moveFocus = useCallback((current: string, dx: number, dy: number) => {
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
  }, []);

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
              <span className="swarm-term-pane-role">{persona?.role ?? '—'}</span>
              <LifecyclePill phase={phase} agentId={agentId} />
              {paneId && (
                <button
                  type="button"
                  className="swarm-term-pane-expand"
                  title={isExpanded ? 'Restore grid (Esc)' : 'Expand pane'}
                  onClick={() => setExpandedAgent(isExpanded ? null : agentId)}
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
  );
}

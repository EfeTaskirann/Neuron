import { NIcon } from '../icons';
import { useTerminalDelete } from '../../hooks/mutations';
import type { Pane } from '../../lib/bindings';
import { isLiveSwarmPane, metaFor } from './agentMeta';

// Horizontal tab strip — every pane gets a chip with status dot,
// agent name, optional role, and a kill button. Only one pane is
// mounted in the body at a time (see `PaneView`), so this scales to
// the 50+ panes a long-running session accumulates without melting
// xterm.js. Scrollable when the strip overflows.
interface PaneTabStripProps {
  panes: Pane[];
  activeId: string;
  onSelect: (id: string) => void;
  /** Pane ids the running swarm session owns (stopped from Terminal Swarm). */
  liveSwarmPaneIds: ReadonlySet<string>;
}

export function PaneTabStrip({
  panes,
  activeId,
  onSelect,
  liveSwarmPaneIds,
}: PaneTabStripProps): JSX.Element {
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
        // A live pane of the Terminal Swarm is stopped from there: closing
        // it here would take one agent out from under the running session.
        const swarmOwned = isLiveSwarmPane(p, liveSwarmPaneIds, p.id);
        return (
          // div, not button: the close affordance nested inside must be
          // a REAL <button> (it's the only caller of terminal:delete —
          // PaneHeader close just flips status), and interactive
          // elements can't nest. Keyboard select comes from the
          // role/tabIndex/onKeyDown trio.
          <div
            key={p.id}
            role="tab"
            aria-selected={isActive}
            tabIndex={0}
            className={`pane-tab status-${p.status}${isActive ? ' active' : ''}`}
            onClick={() => onSelect(p.id)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                onSelect(p.id);
              }
            }}
            title={`${meta.name} · ${p.cwd}${swarmOwned ? ' · Terminal Swarm’ın pane’i' : ''}`}
          >
            <span className={`pane-tab-dot status-${p.status}`} />
            <span className="pane-tab-name">{meta.name}</span>
            {p.role && <span className="pane-tab-role">· {p.role}</span>}
            {!swarmOwned && (
              <button
                type="button"
                className="pane-tab-close"
                title="Close pane"
                aria-label={`Close ${meta.name} pane`}
                disabled={del.isPending}
                onClick={(e) => {
                  e.stopPropagation();
                  del.mutate(p.id);
                }}
              >
                <NIcon name="close" size={10} />
              </button>
            )}
          </div>
        );
      })}
    </div>
  );
}

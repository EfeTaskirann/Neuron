import { memo } from 'react';
import { useXtermPane } from '../hooks/useXtermPane';

interface Props {
  paneId: string;
  agentId: string;
}

// Thin wrapper around the shared xterm pane hook — the whole
// mount/resize/snapshot/live lifecycle lives in `useXtermPane`
// (extracted from this file's previous near-verbatim copy of
// `Terminal.tsx::PaneBody`). The swarm grid packs 9 panes, hence the
// smaller font.
function SwarmPaneImpl({ paneId }: Props): JSX.Element {
  const containerRef = useXtermPane(paneId, { fontSize: 11 });
  return <div className="swarm-term-xterm" ref={containerRef} />;
}

// Memoized: TerminalSwarmRoute re-renders on its routing-edge (~1.5 s)
// and lifecycle (5 s) timers. `paneId`/`agentId` are stable for a pane's
// lifetime, so memo keeps those parent ticks from reconciling all 9
// xterm panes for nothing.
export const SwarmPane = memo(SwarmPaneImpl);

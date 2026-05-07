// `SwarmRoute` — top-level orchestrator for the Swarm surface.
//
// W4-04 swap: the W3-12k3 chat-shaped layout (chat panel + recent
// jobs + selected job detail) becomes one of TWO views, gated by a
// tab switcher:
//   - "Live grid"   — the W4-04 3×3 SwarmAgentGrid (default)
//   - "Recent jobs" — the W3-12k3 chat panel + jobs triple
//
// Workspace is the constant `"default"` per WP-W3-14 §2 — multi-
// workspace UX is post-W4.
import { useState } from 'react';
import { OrchestratorChatPanel } from '../components/OrchestratorChatPanel';
import { SwarmJobList } from '../components/SwarmJobList';
import { SwarmJobDetail } from '../components/SwarmJobDetail';
import { SwarmAgentGrid } from '../components/SwarmAgentGrid';

const WORKSPACE_ID = 'default';

type SwarmView = 'grid' | 'jobs';

export function SwarmRoute(): JSX.Element {
  const [view, setView] = useState<SwarmView>('grid');
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  return (
    <div className="route route-swarm">
      <div className="swarm-toolbar">
        <div className="seg swarm-view-seg">
          <button
            type="button"
            className={view === 'grid' ? 'active' : ''}
            onClick={() => setView('grid')}
            aria-pressed={view === 'grid'}
          >
            Live grid
          </button>
          <button
            type="button"
            className={view === 'jobs' ? 'active' : ''}
            onClick={() => setView('jobs')}
            aria-pressed={view === 'jobs'}
          >
            Recent jobs
          </button>
        </div>
      </div>
      {view === 'grid' ? (
        <SwarmAgentGrid workspaceId={WORKSPACE_ID} />
      ) : (
        <div className="swarm-jobs-view">
          <div className="swarm-pane swarm-pane-left">
            <OrchestratorChatPanel
              workspaceId={WORKSPACE_ID}
              onSelectJob={setSelectedJobId}
            />
            <div className="swarm-list-title">Recent jobs</div>
            <SwarmJobList
              workspaceId={WORKSPACE_ID}
              selectedJobId={selectedJobId}
              onSelect={setSelectedJobId}
            />
          </div>
          <div className="swarm-pane swarm-pane-right">
            <SwarmJobDetail jobId={selectedJobId} workspaceId={WORKSPACE_ID} />
          </div>
        </div>
      )}
    </div>
  );
}

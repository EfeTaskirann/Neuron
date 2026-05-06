// `SwarmRoute` — top-level orchestrator for the Swarm surface.
// Two-pane layout (mirrors `RunsRoute`'s page-level structure):
// left = Orchestrator chat panel + recent-jobs list; right =
// selected job detail with live FSM state.
//
// W3-12k3 swap: the W3-14 SwarmGoalForm is replaced by the
// chat-shaped OrchestratorChatPanel. Dispatch outcomes auto-
// chain into `swarm:run_job` and surface the resulting job id
// as a clickable bubble that drives `selectedJobId` (so the
// right pane loads the new job's detail).
//
// Workspace is the constant `"default"` per WP-W3-14 §2 — multi-
// workspace UX is post-W3.
import { useState } from 'react';
import { OrchestratorChatPanel } from '../components/OrchestratorChatPanel';
import { SwarmJobList } from '../components/SwarmJobList';
import { SwarmJobDetail } from '../components/SwarmJobDetail';

const WORKSPACE_ID = 'default';

export function SwarmRoute(): JSX.Element {
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  return (
    <div className="route route-swarm">
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
  );
}

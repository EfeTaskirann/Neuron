// `SwarmRoute` — top-level orchestrator for the W3-14 swarm
// surface. Two-pane layout (mirrors `RunsRoute`'s page-level
// structure): left = goal-input form + recent-jobs list; right
// = selected job detail with live FSM state.
//
// Workspace is the constant `"default"` per WP-W3-14 §2 — multi-
// workspace UX is post-W3.
import { useState } from 'react';
import { SwarmGoalForm } from '../components/SwarmGoalForm';
import { SwarmJobList } from '../components/SwarmJobList';
import { SwarmJobDetail } from '../components/SwarmJobDetail';

const WORKSPACE_ID = 'default';

export function SwarmRoute(): JSX.Element {
  const [selectedJobId, setSelectedJobId] = useState<string | null>(null);
  return (
    <div className="route route-swarm">
      <div className="swarm-pane swarm-pane-left">
        <SwarmGoalForm workspaceId={WORKSPACE_ID} />
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

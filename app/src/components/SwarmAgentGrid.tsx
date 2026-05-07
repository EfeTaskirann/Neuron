// `SwarmAgentGrid` — 3×3 layout of `AgentPane`s (WP-W4-04 §4).
//
// Slot mapping is fixed; rationale in WP-W4-overview §"WP-W4-04
// scope rationale":
//   row 1: Orchestrator | Coordinator | Scout       (the brains + investigator)
//   row 2: Planner      | BackendBuilder | FrontendBuilder
//   row 3: BackendReviewer | FrontendReviewer | Tester
//
// Status pills are sourced from `useAgentStatuses(workspaceId)`
// (polled every 2s); event transcripts come per-pane via
// `useAgentEvents`.
//
// Each pane is keyed by `${workspaceId}:${agentId}` so React
// remounts on workspace change — that resets `useAgentEvents`'
// internal state which deliberately doesn't reset on prop change
// (per the W4-03 hook design note).
import { useMemo } from 'react';
import { AgentPane } from './AgentPane';
import { useAgentStatuses } from '../hooks/useAgentStatuses';
import type { AgentStatusRow } from '../lib/bindings';

interface SlotSpec {
  agentId: string;
  displayName: string;
}

const GRID_SLOTS: SlotSpec[] = [
  { agentId: 'orchestrator', displayName: 'Orchestrator' },
  { agentId: 'coordinator', displayName: 'Coordinator' },
  { agentId: 'scout', displayName: 'Scout' },
  { agentId: 'planner', displayName: 'Planner' },
  { agentId: 'backend-builder', displayName: 'Backend Builder' },
  { agentId: 'frontend-builder', displayName: 'Frontend Builder' },
  { agentId: 'backend-reviewer', displayName: 'Backend Reviewer' },
  { agentId: 'frontend-reviewer', displayName: 'Frontend Reviewer' },
  { agentId: 'integration-tester', displayName: 'Tester' },
];

interface Props {
  workspaceId: string;
}

export function SwarmAgentGrid({ workspaceId }: Props): JSX.Element {
  const { data: statusRows = [] } = useAgentStatuses(workspaceId);
  const statusByAgent = useMemo(
    () => indexByAgentId(statusRows),
    [statusRows],
  );

  return (
    <div className="swarm-grid">
      {GRID_SLOTS.map((slot) => (
        <AgentPane
          key={`${workspaceId}:${slot.agentId}`}
          workspaceId={workspaceId}
          agentId={slot.agentId}
          displayName={slot.displayName}
          status={statusByAgent.get(slot.agentId) ?? null}
        />
      ))}
    </div>
  );
}

function indexByAgentId(
  rows: AgentStatusRow[],
): Map<string, AgentStatusRow> {
  const m = new Map<string, AgentStatusRow>();
  for (const r of rows) {
    m.set(r.agentId, r);
  }
  return m;
}

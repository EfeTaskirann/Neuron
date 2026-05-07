// `useAgentStatuses(workspaceId)` — read-side hook for the
// W4-02 `swarm:agents:list_status` IPC.
//
// The W4-04 grid panes get their status pills from this query.
// `staleTime` is short (1s) because status flips quickly during
// active turns; without polling the user would only see status
// changes when re-mounting. We DON'T derive status from
// `useAgentEvents` because:
//   - the event channel only emits when sessions are alive; a
//     `NotSpawned` slot would never get a status without a poll
//   - the registry's `list_status` is the canonical truth (slot
//     metadata + status enum); deriving in two places risks drift
//
// 2s `refetchInterval` matches the W3-12b job-list cadence, low
// enough that pane status feels live (eye picks up >100ms easily;
// 2s for status is acceptable given event-driven transcript fills
// the perceptual gap during active turns).
import { useQuery } from '@tanstack/react-query';
import { commands, type AgentStatusRow } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

const REFETCH_MS = 2000;

export function useAgentStatuses(workspaceId: string) {
  return useQuery<AgentStatusRow[]>({
    queryKey: ['swarm-agent-statuses', workspaceId],
    queryFn: () => unwrap(commands.swarmAgentsListStatus(workspaceId)),
    staleTime: 1000,
    refetchInterval: REFETCH_MS,
  });
}

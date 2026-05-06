// `useSwarmJobs(workspaceId, limit)` — recent-jobs list backed by
// `swarm:list_jobs` (W3-12b). Light 5s polling keeps the surface
// fresh in the absence of events; the per-job event subscription
// in `useSwarmJob` invalidates this query on `finished` so the
// transition from running → done shows up instantly without
// waiting for the next poll tick.
import { useQuery } from '@tanstack/react-query';
import { commands, type JobSummary } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useSwarmJobs(workspaceId?: string, limit?: number) {
  return useQuery<JobSummary[]>({
    queryKey: ['swarm-jobs', workspaceId ?? null, limit ?? null],
    queryFn: () =>
      unwrap(commands.swarmListJobs(workspaceId ?? null, limit ?? null)),
    refetchInterval: 5_000,
  });
}

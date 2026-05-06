// `useOrchestratorHistory(workspaceId)` — read-side hook for the
// persisted Orchestrator chat thread (WP-W3-12k2 §7). Mirrors the
// `useSwarmJobs` shape: tagged-union IPC unwrapped via `unwrap`,
// keyed by workspaceId so a future multi-workspace UI can swap
// without churning the cache key strategy.
//
// `staleTime: Infinity` — history is loaded once on mount; mutations
// (`useOrchestratorDecide`, `useLogOrchestratorJob`,
// `useClearOrchestratorHistory`) invalidate the query explicitly so
// the cache stays correct without polling.
import { useQuery } from '@tanstack/react-query';
import { commands, type OrchestratorMessage } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useOrchestratorHistory(workspaceId: string) {
  return useQuery<OrchestratorMessage[]>({
    queryKey: ['orchestrator-history', workspaceId],
    queryFn: () => unwrap(commands.swarmOrchestratorHistory(workspaceId, null)),
    staleTime: Infinity,
  });
}

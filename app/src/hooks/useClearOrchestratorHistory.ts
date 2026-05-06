// `useClearOrchestratorHistory()` — mutation hook for the chat
// panel's "Clear chat" button (WP-W3-12k2 §5). Backend hard-deletes
// every persisted message for the workspace; the panel resets its
// local `messages` state to empty and we invalidate the cached
// history query so subsequent mounts see the empty thread.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useClearOrchestratorHistory() {
  const qc = useQueryClient();
  return useMutation<null, Error, string /* workspaceId */>({
    mutationFn: (workspaceId) =>
      unwrap(commands.swarmOrchestratorClearHistory(workspaceId)),
    onSettled: (_data, _err, workspaceId) => {
      qc.invalidateQueries({
        queryKey: ['orchestrator-history', workspaceId],
      });
    },
  });
}

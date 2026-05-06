// `useLogOrchestratorJob()` — mutation hook called from the chat
// panel after a `dispatch` outcome lands a new swarm job
// (WP-W3-12k2 §6). Persists a `role=job` row in the chat thread so
// the next mount renders the dispatched-job bubble between the
// orchestrator outcome and the user's next message.
//
// Frontend orchestrates: `decide` → `runJob` → `logJob` → invalidate
// history. Failure of the log step is non-fatal — the in-memory
// bubble still shows; only the persisted thread misses the row.
// Documented as a known race in WP §"Notes / risks".
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export interface LogOrchestratorJobInput {
  workspaceId: string;
  jobId: string;
  goal: string;
}

export function useLogOrchestratorJob() {
  const qc = useQueryClient();
  return useMutation<null, Error, LogOrchestratorJobInput>({
    mutationFn: ({ workspaceId, jobId, goal }) =>
      unwrap(commands.swarmOrchestratorLogJob(workspaceId, jobId, goal)),
    onSettled: (_data, _err, vars) => {
      qc.invalidateQueries({
        queryKey: ['orchestrator-history', vars.workspaceId],
      });
    },
  });
}

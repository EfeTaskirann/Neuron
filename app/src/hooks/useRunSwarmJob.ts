// `useRunSwarmJob()` — kick off a swarm job (W3-12a). The IPC
// blocks until the FSM finishes (Done / Failed); the UI does not
// `await` the mutation's promise — instead it relies on the
// per-job event channel (consumed by `useSwarmJob`) for
// mid-flight transitions, and on `onSettled` invalidating
// `['swarm-jobs']` so the new row appears in the list as soon
// as the IPC returns.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { commands, type JobOutcome } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export interface RunSwarmJobInput {
  workspaceId: string;
  goal: string;
}

export function useRunSwarmJob() {
  const qc = useQueryClient();
  return useMutation<JobOutcome, Error, RunSwarmJobInput>({
    mutationFn: ({ workspaceId, goal }) =>
      unwrap(commands.swarmRunJob(workspaceId, goal)),
    onSettled: () => {
      qc.invalidateQueries({ queryKey: ['swarm-jobs'] });
    },
  });
}

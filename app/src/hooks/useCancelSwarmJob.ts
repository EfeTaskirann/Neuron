// `useCancelSwarmJob()` — signal cancellation for an in-flight
// swarm job (W3-12c). Backend returns `Ok(())` on success or
// `Err(NotFound|Conflict)` for unknown / terminal jobs; both
// error paths surface as a thrown `Error` via `unwrap`.
//
// Invalidating both the per-job and the list queries on settle
// covers the two visible side-effects: the detail pane flips
// to Failed, and the list row's status pill updates.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useCancelSwarmJob() {
  const qc = useQueryClient();
  return useMutation<null, Error, string /* jobId */>({
    mutationFn: (jobId) => unwrap(commands.swarmCancelJob(jobId)),
    onSettled: (_data, _err, jobId) => {
      qc.invalidateQueries({ queryKey: ['swarm-job', jobId] });
      qc.invalidateQueries({ queryKey: ['swarm-jobs'] });
    },
  });
}

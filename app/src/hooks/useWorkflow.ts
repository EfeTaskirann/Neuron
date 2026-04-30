import { useQuery } from '@tanstack/react-query';
import { commands, type WorkflowDetail } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `workflows:get` — full detail (workflow row + nodes + edges) for
// the canvas to render. Cache key includes the id so multi-workflow
// projects (Week 3) get per-id caching for free.
export function useWorkflow(id: string) {
  return useQuery<WorkflowDetail>({
    queryKey: ['workflow', id],
    queryFn: () => unwrap(commands.workflowsGet(id)),
    enabled: !!id,
  });
}

import { useQuery } from '@tanstack/react-query';
import { commands, type Workflow } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useWorkflows() {
  return useQuery<Workflow[]>({
    queryKey: ['workflows'],
    queryFn: () => unwrap(commands.workflowsList()),
  });
}

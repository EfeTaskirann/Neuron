import { useQuery } from '@tanstack/react-query';
import { commands, type Agent } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useAgents() {
  return useQuery<Agent[]>({
    queryKey: ['agents'],
    queryFn: () => unwrap(commands.agentsList()),
  });
}

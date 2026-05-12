import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type SwarmTermPersona } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useSwarmTermPersonas() {
  return useQuery<SwarmTermPersona[]>({
    queryKey: ['swarm-term', 'personas'],
    queryFn: () => unwrap(commands.swarmTermListPersonas()),
    staleTime: Infinity,
  });
}

export function useSwarmTermSessionStatus() {
  return useQuery({
    queryKey: ['swarm-term', 'status'],
    queryFn: () => unwrap(commands.swarmTermSessionStatus()),
  });
}

export function useStartSwarmTermSession() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (projectDir: string) =>
      unwrap(commands.swarmTermStartSession(projectDir)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
      qc.invalidateQueries({ queryKey: ['panes'] });
    },
  });
}

export function useStopSwarmTermSession() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => unwrap(commands.swarmTermStopSession()),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
      qc.invalidateQueries({ queryKey: ['panes'] });
    },
  });
}

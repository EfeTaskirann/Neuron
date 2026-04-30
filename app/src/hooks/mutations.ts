// Mutation hooks for the command surface. Each one calls a Tauri
// command via `commands.X()`, unwraps the tagged-union result, and
// invalidates the relevant list query on success per ADR-0005.
//
// All mutations live here rather than per-domain files because
// the bodies are small and the import surface stays compact —
// promote into per-domain modules if the count grows past ~12.
import { useMutation, useQueryClient } from '@tanstack/react-query';
import {
  commands,
  type Agent,
  type AgentCreateInput,
  type AgentPatch,
  type Run,
  type Server,
} from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useAgentCreate() {
  const qc = useQueryClient();
  return useMutation<Agent, Error, AgentCreateInput>({
    mutationFn: (input) => unwrap(commands.agentsCreate(input)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['agents'] });
    },
  });
}

export function useAgentUpdate() {
  const qc = useQueryClient();
  return useMutation<Agent, Error, { id: string; patch: AgentPatch }>({
    mutationFn: ({ id, patch }) => unwrap(commands.agentsUpdate(id, patch)),
    onSuccess: (_data, { id }) => {
      qc.invalidateQueries({ queryKey: ['agents'] });
      qc.invalidateQueries({ queryKey: ['agent', id] });
    },
  });
}

export function useAgentDelete() {
  const qc = useQueryClient();
  return useMutation<null, Error, string>({
    mutationFn: (id) => unwrap(commands.agentsDelete(id)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['agents'] });
    },
  });
}

export function useRunCreate() {
  const qc = useQueryClient();
  return useMutation<Run, Error, string>({
    mutationFn: (workflowId) => unwrap(commands.runsCreate(workflowId)),
    onSuccess: () => {
      // Both the list and the per-run snapshot need refreshing
      // — the new run's id is in the response but the inspector
      // re-renders via useRuns picking up the freshest entry.
      qc.invalidateQueries({ queryKey: ['runs'] });
    },
  });
}

export function useMcpInstall() {
  const qc = useQueryClient();
  return useMutation<Server, Error, string>({
    mutationFn: (id) => unwrap(commands.mcpInstall(id)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['servers'] });
    },
  });
}

export function useMcpUninstall() {
  const qc = useQueryClient();
  return useMutation<Server, Error, string>({
    mutationFn: (id) => unwrap(commands.mcpUninstall(id)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['servers'] });
    },
  });
}

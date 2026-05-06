// `useOrchestratorDecide()` — single-shot Orchestrator decision
// (WP-W3-12k1 §3). The IPC returns an `OrchestratorOutcome`
// tagging the assistant's chosen action (`direct_reply` /
// `clarify` / `dispatch`); the consumer (OrchestratorChatPanel)
// branches off `action` to decide whether to render a bot
// bubble, ask a clarifying question, or chain into
// `swarm:run_job`.
//
// Mirrors the `useRunSwarmJob` shape: `unwrap` flips the tagged
// `{status:'ok'|'error'}` envelope into a thrown Error for the
// failure path so TanStack Query's mutation lifecycle catches it.
//
// Stateless per W3-12k1 contract — no list invalidation needed
// (chat history lives in component state until W3-12k-2).
import { useMutation } from '@tanstack/react-query';
import { commands, type OrchestratorOutcome } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export interface OrchestratorDecideInput {
  workspaceId: string;
  userMessage: string;
}

export function useOrchestratorDecide() {
  return useMutation<OrchestratorOutcome, Error, OrchestratorDecideInput>({
    mutationFn: ({ workspaceId, userMessage }) =>
      unwrap(commands.swarmOrchestratorDecide(workspaceId, userMessage)),
  });
}

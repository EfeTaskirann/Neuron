// `useLogOrchestratorJob()` — mutation hook called from the chat
// panel after a `dispatch` outcome lands a new swarm job
// (WP-W3-12k2 §6). Persists a `role=job` row in the chat thread so
// the next mount renders the dispatched-job bubble between the
// orchestrator outcome and the user's next message.
//
// Frontend orchestrates: `decide` → `runJob` → `logJob`. Failure of
// the log step is non-fatal — the in-memory bubble still shows;
// only the persisted thread misses the row. Documented as a known
// race in WP §"Notes / risks".
//
// Why: a mid-session invalidate would refetch the history (now
// containing the just-persisted user + orchestrator + job rows)
// while the panel's `localMessages` still holds the same three
// bubbles, doubling everything on screen. The panel relies on its
// mount-time fetch to pick up persistence on the *next* mount —
// see `OrchestratorChatPanel.tsx` ("we deliberately do NOT
// invalidate `['orchestrator-history']` mid-session").
import { useMutation } from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export interface LogOrchestratorJobInput {
  workspaceId: string;
  jobId: string;
  goal: string;
}

export function useLogOrchestratorJob() {
  return useMutation<null, Error, LogOrchestratorJobInput>({
    mutationFn: ({ workspaceId, jobId, goal }) =>
      unwrap(commands.swarmOrchestratorLogJob(workspaceId, jobId, goal)),
  });
}

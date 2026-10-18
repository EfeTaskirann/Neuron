import {
  keepPreviousData,
  useMutation,
  useQuery,
  useQueryClient,
  type QueryClient,
} from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// The Changes panel: what the agents changed in the project since its
// checkpoint (taken when the swarm started there), each file's diff, and
// the way back. See `swarm_term::checkpoint` on the Rust side.

/** How often the list is refreshed while a session runs. */
const CHANGES_POLL_MS = 15_000;

const changesKey = (projectDir: string | null) => ['swarm-term', 'changes', projectDir] as const;

/** The change list, read now (shared with the hook's cache). */
export function fetchSwarmTermChanges(qc: QueryClient, projectDir: string) {
  return qc.fetchQuery({
    queryKey: changesKey(projectDir),
    queryFn: () => unwrap(commands.swarmTermChanges(projectDir)),
    staleTime: 0,
  });
}

/** What changed since the checkpoint; polled while the agents work (`live`). */
export function useSwarmTermChanges(
  projectDir: string | null,
  { enabled, live }: { enabled: boolean; live: boolean },
) {
  return useQuery({
    queryKey: changesKey(projectDir),
    queryFn: () => unwrap(commands.swarmTermChanges(projectDir!)),
    enabled: enabled && projectDir != null,
    refetchInterval: live ? CHANGES_POLL_MS : false,
  });
}

/** One file's diff; `version` refetches it when the list is refreshed. */
export function useSwarmTermChangeDiff(
  projectDir: string | null,
  path: string | null,
  version: number,
) {
  return useQuery({
    queryKey: ['swarm-term', 'change-diff', projectDir, path, version],
    queryFn: () => unwrap(commands.swarmTermChangeDiff(projectDir!, path!)),
    enabled: projectDir != null && path != null,
    // A list poll bumps `version`, so the diff refetches; keep the last
    // diff shown meanwhile instead of blanking and resetting the scroll.
    placeholderData: keepPreviousData,
  });
}

/** Put files back as they were at the checkpoint (`null` = every one). */
export function useRevertSwarmTermChanges(projectDir: string | null) {
  const qc = useQueryClient();
  return useMutation({
    // The Changes panel shows the failure in its own error line.
    meta: { errorsShownInline: true },
    mutationFn: (paths: string[] | null) =>
      unwrap(commands.swarmTermRevertChanges(projectDir!, paths)),
    onSuccess: (left) => {
      qc.setQueryData(changesKey(projectDir), left);
      qc.invalidateQueries({ queryKey: ['swarm-term', 'change-diff'] });
    },
  });
}

/** Keep what changed: the checkpoint moves to now. */
export function useKeepSwarmTermChanges(projectDir: string | null) {
  const qc = useQueryClient();
  return useMutation({
    meta: { errorsShownInline: true },
    mutationFn: () => unwrap(commands.swarmTermKeepChanges(projectDir!)),
    onSuccess: (left) => {
      qc.setQueryData(changesKey(projectDir), left);
      qc.invalidateQueries({ queryKey: ['swarm-term', 'change-diff'] });
    },
  });
}

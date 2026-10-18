import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type SwarmTermPersona } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { dismissToast, showToast } from '../lib/toast';
import { useTauriEvent } from './useTauriEvent';

export function useSwarmTermPersonas() {
  return useQuery<SwarmTermPersona[]>({
    queryKey: ['swarm-term', 'personas'],
    queryFn: () => unwrap(commands.swarmTermListPersonas()),
    staleTime: Infinity,
  });
}

export function useSwarmTermSessionStatus() {
  const qc = useQueryClient();
  // The handle carries every pane's readiness: refetch it when a pane's
  // state changes or a session starts / stops anywhere (not only after
  // this window's own start / stop mutations).
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
  };
  useTauriEvent('swarm-term:pane-state', refresh);
  useTauriEvent('swarm-term:session', refresh);
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

/** The user's yes to running the swarm in a folder (see `consent_required`). */
export function useTrustSwarmTermProject() {
  return useMutation({
    mutationFn: (projectDir: string) =>
      unwrap(commands.swarmTermTrustProject(projectDir)),
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

export function useRunClaudeUpdate() {
  return useMutation({
    mutationFn: () => unwrap(commands.swarmTermRunUpdate()),
    // `onMutate` returns a context object that TanStack threads into
    // `onSuccess` / `onError`, so the per-invocation toast id outlives
    // the closure without a module-level ref.
    onMutate: () => {
      const toastId = showToast({
        variant: 'info',
        body: 'Claude güncelleniyor… (30-60sn sürebilir)',
        durationMs: null,
      });
      return { toastId };
    },
    onSuccess: (data, _vars, ctx) => {
      dismissToast(ctx.toastId);
      if (data.exitCode === 0) {
        showToast({
          variant: 'success',
          body: 'Claude güncellendi',
          durationMs: 5000,
        });
      } else {
        const tail = data.stderrTail ?? '';
        const lastLines = tail
          .split('\n')
          .filter((l) => l.length > 0)
          .slice(-2)
          .join('\n');
        showToast({
          variant: 'error',
          body: lastLines || `Update failed (exit=${data.exitCode})`,
          durationMs: 6000,
        });
      }
    },
    onError: (err, _vars, ctx) => {
      if (ctx) dismissToast(ctx.toastId);
      const msg = err instanceof Error ? err.message : String(err);
      showToast({
        variant: 'error',
        body: msg || 'Update failed',
        durationMs: 6000,
      });
    },
  });
}

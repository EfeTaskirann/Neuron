import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import {
  commands,
  type PaneReadiness,
  type SwarmTermPersona,
  type SwarmTermTeam,
} from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { notifyBudgetReached, notifyPaneExited, notifyUsageLimit } from '../lib/swarmNotify';
import { dismissToast, showToast } from '../lib/toast';
import { useTauriEvent } from './useTauriEvent';

/** `swarm-term:session` payload (see `session.rs`). */
interface SessionEvent {
  sessionId: string;
  state: 'started' | 'stopped' | 'limited' | 'paused' | 'budget' | 'running' | 'tokens';
  /** With `limited`: when claude said the limit resets (unix seconds). */
  resetsAt?: number | null;
}

/** `swarm-term:pane-state` payload (see `pane_state.rs`). */
interface PaneStateEvent {
  sessionId: string;
  agentId: string;
  readiness: PaneReadiness;
  detail: string | null;
}

export function useSwarmTermPersonas() {
  return useQuery<SwarmTermPersona[]>({
    queryKey: ['swarm-term', 'personas'],
    queryFn: () => unwrap(commands.swarmTermListPersonas()),
    staleTime: Infinity,
  });
}

/** The session handle. Freshness comes from [`useSwarmTermAlerts`], which
 *  runs app-wide and invalidates this shared key on every session /
 *  pane-state event — so the route and the shell agree whatever tab is
 *  open. */
function sessionStatusQuery() {
  return {
    queryKey: ['swarm-term', 'status'] as const,
    queryFn: () => unwrap(commands.swarmTermSessionStatus()),
  };
}

export function useSwarmTermSessionSnapshot() {
  return useQuery(sessionStatusQuery());
}

export function useSwarmTermSessionStatus() {
  return useQuery(sessionStatusQuery());
}

/**
 * App-wide (mounted once in <App>): tell the user — even on another tab
 * or with the window in the background — that the usage limit or the
 * message budget holds the swarm, or that an agent's claude exited; and
 * keep the shared session-status query fresh for every consumer (the
 * route's readiness, the shell's close / project-switch guards).
 */
export function useSwarmTermAlerts(): void {
  const qc = useQueryClient();
  const refresh = () => qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
  useTauriEvent<SessionEvent>('swarm-term:session', (e) => {
    refresh();
    if (e.state === 'limited') notifyUsageLimit(e.resetsAt ?? null);
    if (e.state === 'budget') notifyBudgetReached();
  });
  useTauriEvent<PaneStateEvent>('swarm-term:pane-state', (e) => {
    refresh();
    if (e.readiness === 'exited') notifyPaneExited(e.agentId);
  });
}

// The Terminal Swarm route reports every session action's failure in its
// own error banner: `errorsShownInline` keeps the global toast off them.
const INLINE = { errorsShownInline: true } as const;

export function useStartSwarmTermSession() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
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
    meta: INLINE,
    mutationFn: (projectDir: string) =>
      unwrap(commands.swarmTermTrustProject(projectDir)),
  });
}

export function useStopSwarmTermSession() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
    mutationFn: () => unwrap(commands.swarmTermStopSession()),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
      qc.invalidateQueries({ queryKey: ['panes'] });
    },
  });
}

/** Run one agent again in a new pane, on the same conversation. */
export function useRestartSwarmTermAgent() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
    mutationFn: (agentId: string) => unwrap(commands.swarmTermRestartAgent(agentId)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
      qc.invalidateQueries({ queryKey: ['panes'] });
    },
  });
}

/** The user's "go on" while the usage limit holds the swarm. */
export function useResumeSwarmTermDeliveries() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
    mutationFn: () => unwrap(commands.swarmTermResumeDeliveries()),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
    },
  });
}

/** Pause / resume the running swarm's deliveries. */
export function useSetSwarmTermPaused() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
    mutationFn: (paused: boolean) => unwrap(commands.swarmTermSetPaused(paused)),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
    },
  });
}

/** More messages for a session whose budget is used up. */
export function useExtendSwarmTermBudget() {
  const qc = useQueryClient();
  return useMutation({
    meta: INLINE,
    mutationFn: () => unwrap(commands.swarmTermExtendBudget()),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'status'] });
    },
  });
}

/** The cost controls (team, models, message budget) — Settings → Models. */
export function useSwarmTermTeam() {
  return useQuery({
    queryKey: ['swarm-term', 'team'],
    queryFn: () => unwrap(commands.swarmTermGetTeam()),
  });
}

export function useSaveSwarmTermTeam() {
  const qc = useQueryClient();
  return useMutation({
    // Settings → Models shows the failure above its rows.
    meta: INLINE,
    mutationFn: (team: SwarmTermTeam) => unwrap(commands.swarmTermSetTeam(team)),
    // Shown at once; a refused value comes back from the backend.
    onMutate: (team) => {
      qc.setQueryData(['swarm-term', 'team'], team);
    },
    onSettled: () => {
      qc.invalidateQueries({ queryKey: ['swarm-term', 'team'] });
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

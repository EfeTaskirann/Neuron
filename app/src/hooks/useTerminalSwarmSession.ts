import { useCallback, useEffect, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type SwarmTermPersona } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { dismissToast, showToast } from '../lib/toast';

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

const AUTONOMOUS_STORAGE_KEY = 'swarm-term:autonomous';
const AUTONOMOUS_BODY_ATTR = 'data-swarm-autonomous';
const AUTONOMOUS_EVENT = 'swarm-term:autonomous-change';

interface AutonomousChangeDetail {
  value: boolean;
}

/**
 * Persisted "Run autonomously" toggle. While ON the swarm runs
 * end-to-end without the operator having to approve specialist
 * dispatches; the frontend annotates `<body data-swarm-autonomous="1">`
 * so any approval-prompt host elsewhere in the app can suppress its
 * modal at render time, and the operator gets a visible AUTO chip in
 * the swarm-term toolbar.
 *
 * State is mirrored to localStorage so a reload preserves the
 * operator's intent across sessions, and a same-tab CustomEvent keeps
 * multiple consumers (toolbar + any future inline indicator) in sync
 * without prop-drilling.
 */
export function useAutonomousMode(): [boolean, (next: boolean) => void] {
  const [enabled, setEnabled] = useState<boolean>(() => {
    if (typeof window === 'undefined') return false;
    try {
      return window.localStorage.getItem(AUTONOMOUS_STORAGE_KEY) === '1';
    } catch {
      return false;
    }
  });

  // Mirror state onto <body> so non-React approval surfaces and CSS
  // can react to autonomous mode without subscribing to the hook.
  useEffect(() => {
    if (typeof document === 'undefined') return;
    if (enabled) {
      document.body.setAttribute(AUTONOMOUS_BODY_ATTR, '1');
    } else {
      document.body.removeAttribute(AUTONOMOUS_BODY_ATTR);
    }
  }, [enabled]);

  // Cross-instance sync: another tab (storage event) or another
  // component in this tab (custom event) flipping the toggle should
  // update every subscriber.
  useEffect(() => {
    if (typeof window === 'undefined') return;
    const onStorage = (e: StorageEvent) => {
      if (e.key !== AUTONOMOUS_STORAGE_KEY) return;
      setEnabled(e.newValue === '1');
    };
    const onCustom = (e: Event) => {
      const detail = (e as CustomEvent<AutonomousChangeDetail>).detail;
      if (detail && typeof detail.value === 'boolean') {
        setEnabled(detail.value);
      }
    };
    window.addEventListener('storage', onStorage);
    window.addEventListener(AUTONOMOUS_EVENT, onCustom);
    return () => {
      window.removeEventListener('storage', onStorage);
      window.removeEventListener(AUTONOMOUS_EVENT, onCustom);
    };
  }, []);

  const setValue = useCallback((next: boolean) => {
    setEnabled(next);
    if (typeof window === 'undefined') return;
    try {
      window.localStorage.setItem(AUTONOMOUS_STORAGE_KEY, next ? '1' : '0');
    } catch {
      /* private mode / quota — body attr + in-memory state still hold */
    }
    window.dispatchEvent(
      new CustomEvent<AutonomousChangeDetail>(AUTONOMOUS_EVENT, {
        detail: { value: next },
      }),
    );
  }, []);

  return [enabled, setValue];
}

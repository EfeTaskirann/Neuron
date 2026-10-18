import { MutationCache, QueryClient } from '@tanstack/react-query';
import { showToast } from './toast';
import { AppErrorClient } from './unwrap';

declare module '@tanstack/react-query' {
  interface Register {
    mutationMeta: {
      /** The caller reports this mutation's failures itself (inline). */
      errorsShownInline?: boolean;
    };
  }
}

/**
 * A failed action as an error toast: the kind's line, with the backend's
 * own words (which folder, which server, git's complaint) as its detail.
 */
export function toastError(err: unknown): void {
  if (err instanceof AppErrorClient) {
    showToast({
      variant: 'error',
      body: err.message,
      detail: err.detail && err.detail !== err.message ? err.detail : undefined,
      durationMs: 10_000,
    });
    return;
  }
  showToast({
    variant: 'error',
    body: err instanceof Error ? err.message : String(err),
    durationMs: 10_000,
  });
}

// ADR-0005 defaults: lists go stale after 30s, evicted after 5min
// idle, and a single retry covers transient errors without
// hammering the backend during outages. `refetchOnWindowFocus` is
// off because Tauri windows are always focused-ish; the live-event
// pattern (ADR-0006) keeps caches fresh, not focus polling.
export const queryClient = new QueryClient({
  // No failed action is silent: a mutation whose caller neither handles
  // nor shows its errors gets an error toast.
  mutationCache: new MutationCache({
    onError: (err, _vars, _ctx, mutation) => {
      if (mutation.meta?.errorsShownInline || mutation.options.onError) return;
      toastError(err);
    },
  }),
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: 5 * 60_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

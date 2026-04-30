import { QueryClient } from '@tanstack/react-query';

// ADR-0005 defaults: lists go stale after 30s, evicted after 5min
// idle, and a single retry covers transient errors without
// hammering the backend during outages. `refetchOnWindowFocus` is
// off because Tauri windows are always focused-ish; the live-event
// pattern (ADR-0006) keeps caches fresh, not focus polling.
export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      gcTime: 5 * 60_000,
      retry: 1,
      refetchOnWindowFocus: false,
    },
  },
});

import { useMemo, useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';

// Shared event-collection hook for the swarm-term routing event
// stream. The backend (`src-tauri/src/swarm_term/bridge/`) emits
// one `swarm-term:route` event per routing attempt with an
// `outcome` discriminant:
//
//   ok                   — message delivered to target pane
//   malformed            — envelope failed JSON parse / validation
//   denied               — hierarchy graph forbids the edge
//   unknown_target       — target agent has no pane in the session
//   target_not_ready     — target pane hasn't answered the kickoff with "@<id> hazır."
//   target_locked        — target pane is in error state (wedged / no stdin)
//   target_write_timeout — write_to_pane exceeded the 2 s cap (delivery.rs)
//   lifecycle_fanout     — bridge-synthesised autonomy follow-up (review /
//                          retest / TASK_DONE / TASK_FAILED fanout)
//   rejected             — a waiting message gave up (target never ready,
//                          stayed locked, or 5 failed writes); moved to rejected/
//   notice               — `[Neuron]` notice pasted into an agent's pane
//                          (source "neuron"): its message was not delivered,
//                          or its lifecycle signal (e.g. "tamam —" instead of
//                          "DONE") was not recognised
//   reminder             — `[Neuron]` stalled-task reminder (source "neuron")
//
// Collection is a MODULE-LEVEL store (mirrors lib/toast.ts): one
// app-lifetime listener starts on the first consumer mount and the
// event array survives unmounts, so the Routing Log keeps its history
// across tab navigation instead of restarting empty. Consumers read
// it via useSyncExternalStore. The backend tracing log remains the
// forensic source of truth; this buffer is display-grade (capped).
// Derived per-agent views (active edge, lifecycle pill) live in
// useAgentLifecycle.ts.

export type RouteOutcome =
  | 'ok'
  | 'malformed'
  | 'denied'
  | 'unknown_target'
  | 'target_not_ready'
  | 'target_locked'
  | 'target_write_timeout'
  | 'lifecycle_fanout'
  | 'rejected'
  | 'notice'
  | 'reminder';

export interface RouteEvent {
  /** Client-minted monotonic id — a stable React list key (rows
   *  prepend, so index/ts keys would re-key every row per event). */
  id: number;
  source: string;
  target: string;
  body: string;
  outcome: RouteOutcome;
  /** Why a non-ok hop happened (denied / rejected / malformed reason).
   *  Present only for non-`ok` outcomes the backend annotates. */
  reason?: string;
  /** Wall-clock receive time (ms since epoch). Stamped client-side
   *  on arrival because the Rust emit doesn't carry a timestamp.
   *  Sufficient for display ordering — for forensic reasoning the
   *  backend's tracing log is authoritative. */
  ts: number;
}

interface RawPayload {
  source: string;
  target: string;
  body?: string;
  outcome: string;
  reason?: string;
}

export const ALL_OUTCOMES: readonly RouteOutcome[] = [
  'ok',
  'malformed',
  'denied',
  'unknown_target',
  'target_not_ready',
  'target_locked',
  'target_write_timeout',
  'lifecycle_fanout',
  'rejected',
  'notice',
  'reminder',
];

function coerceOutcome(raw: string): RouteOutcome {
  return (ALL_OUTCOMES as readonly string[]).includes(raw)
    ? (raw as RouteOutcome)
    : 'unknown_target';
}

// ── Module-level event store ───────────────────────────────────────
// Newest-first, capped. Array identity changes on every push so
// useSyncExternalStore consumers re-render; ids stay unique for the
// app lifetime (stable React keys).
const ROUTE_EVENT_CAP = 500;
let nextRouteEventId = 0;
let routeEvents: RouteEvent[] = [];
const routeStoreListeners = new Set<() => void>();
let routeListenerStarted = false;

function emitRouteStore(): void {
  for (const l of routeStoreListeners) l();
}

function pushRouteEvent(p: RawPayload): void {
  const next: RouteEvent = {
    id: nextRouteEventId++,
    source: p.source,
    target: p.target,
    body: typeof p.body === 'string' ? p.body : '',
    outcome: coerceOutcome(p.outcome),
    reason: typeof p.reason === 'string' ? p.reason : undefined,
    ts: Date.now(),
  };
  const out = [next, ...routeEvents];
  if (out.length > ROUTE_EVENT_CAP) out.length = ROUTE_EVENT_CAP;
  routeEvents = out;
  emitRouteStore();
}

export function clearRoutingEvents(): void {
  routeEvents = [];
  emitRouteStore();
}

// Lazy app-lifetime subscription — started by the first consumer,
// never torn down (the whole point: history outlives any one tab).
// Registration is best-effort, same as useTauriEvent: jsdom has no
// Tauri bridge, so a rejected listen() is logged, not thrown.
function ensureRouteListener(): void {
  if (routeListenerStarted) return;
  routeListenerStarted = true;
  listen<RawPayload>('swarm-term:route', (event) => {
    pushRouteEvent(event.payload);
  }).catch((err) => {
    console.warn('[useRoutingEvents] subscribe failed', err);
  });
}

function subscribeRouteStore(cb: () => void): () => void {
  routeStoreListeners.add(cb);
  return () => {
    routeStoreListeners.delete(cb);
  };
}

function routeStoreSnapshot(): RouteEvent[] {
  return routeEvents;
}

export function useRoutingEvents(maxRows = ROUTE_EVENT_CAP): {
  events: RouteEvent[];
  clear: () => void;
} {
  ensureRouteListener();
  const all = useSyncExternalStore(
    subscribeRouteStore,
    routeStoreSnapshot,
    routeStoreSnapshot,
  );
  // The store caps at ROUTE_EVENT_CAP; a consumer asking for fewer
  // rows gets a memoized slice so its identity is stable per push.
  const events = useMemo(
    () => (all.length > maxRows ? all.slice(0, maxRows) : all),
    [all, maxRows],
  );
  return {
    events,
    clear: clearRoutingEvents,
  };
}

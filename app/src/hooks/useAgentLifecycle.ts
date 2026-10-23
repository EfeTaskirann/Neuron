import { useEffect, useMemo, useState, useSyncExternalStore } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { RouteEvent, RouteOutcome } from './useRoutingEvents';

// Display-grade derivations over the swarm-term routing stream
// collected by `useRoutingEvents`: the hierarchy diagram's active
// edge and the per-agent lifecycle pill. Pure presentation — the
// event store and its Tauri listener stay in useRoutingEvents.ts.

/** Lifecycle phase derived from recent routing activity per agent. */
export type AgentLifecycle =
  | 'idle'
  | 'assigned'
  | 'building'
  | 'review'
  | 'done'
  /** A reviewer / the tester sent the agent's work back (CHANGES_NEEDED). */
  | 'rework'
  /** The agent's task was closed as failed (`hata`, TASK_FAILED). */
  | 'failed';

export interface ActiveEdge {
  source: string;
  target: string;
  body: string;
  outcome: RouteOutcome;
  ts: number;
}

const LIFECYCLE_WINDOW_MS = 60_000;
const REVIEWER_AGENTS = new Set([
  'backend-reviewer',
  'frontend-reviewer',
  'integration-tester',
]);

/**
 * Body-text heuristic that flags an agent's own message as a
 * task-completion signal. Mirrors the 4-state contract documented in
 * the personas under `src-tauri/src/swarm/agents/term/*.md`: agents announce
 * "tamam — …" (TR) / "done — …" / ✓ when their assigned slice is
 * finished. We only need to recognise the START of the body so that
 * mid-message occurrences of the word "tamam" (e.g. quoted) don't
 * mis-flag normal building turns.
 */
const DONE_BODY_PREFIX_RE = /^\s*(?:tamam|done|✓|✔|complete[d]?)\b/i;

/**
 * Identifies the most recent successful route (`ok`) inside a
 * trailing window — used by the hierarchy diagram to glow the active
 * source→target edge. Non-`ok` outcomes are skipped: a `denied` or
 * `unknown_target` event isn't a live conversation, so the diagram
 * stays quiet.
 *
 * Refreshes once a second (cheap useEffect timer) so an edge that
 * fell outside the window decays even if no new event arrives — the
 * filter is `now - ts < ttlMs`, which only changes when `now`
 * advances.
 */
export function useActiveEdge(
  events: RouteEvent[],
  ttlMs = 3_000,
): ActiveEdge | null {
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    // Tick at half the TTL so a stale edge clears within one frame of
    // the user noticing; bound below by 250 ms to keep idle CPU low.
    const interval = Math.max(250, Math.floor(ttlMs / 2));
    const id = window.setInterval(() => setNow(Date.now()), interval);
    return () => window.clearInterval(id);
  }, [ttlMs]);

  return useMemo<ActiveEdge | null>(() => {
    const latest = events.find((ev) => ev.outcome === 'ok');
    if (!latest) return null;
    if (now - latest.ts > ttlMs) return null;
    return {
      source: latest.source,
      target: latest.target,
      body: latest.body,
      outcome: latest.outcome,
      ts: latest.ts,
    };
  }, [events, now, ttlMs]);
}

/**
 * Authoritative per-agent lifecycle phase, sourced from the backend
 * `swarm-term:lifecycle` event (recorded by
 * `swarm_term::lifecycle::LifecycleStore` and emitted by the bridge's
 * `lifecycle_synthesise` path). The backend state machine is the
 * source of truth; the body-text heuristic below can disagree with
 * it, so authoritative state wins on merge.
 *
 * `source` sent the signal; `owners` are the agents whose work the
 * task is — a reviewer's `CHANGES_NEEDED` belongs on the BUILDER's
 * pill. Older backends send no `owners`: the signal's sender then.
 */
interface LifecyclePayload {
  source: string;
  source_pane: string;
  task_id: string;
  transition: string;
  state: string;
  owners?: string[];
}

/** Pure: the phase a backend `(state, transition)` means for its owners. */
export function lifecyclePhase(state: string, transition: string): AgentLifecycle | null {
  switch (state) {
    case 'Assigned':
      return 'assigned';
    case 'Building':
      return 'building';
    case 'AwaitingReview':
      return 'review';
    case 'Approved':
    case 'Done':
      return 'done';
    case 'Failed':
      // A rejection, or a task held for the user (`Blocked`): a fix is
      // coming, the task is not lost.
      return transition === 'ChangesNeeded' || transition === 'Blocked' ? 'rework' : 'failed';
    default:
      return null;
  }
}

const VERDICTS = new Set(['Approved', 'ChangesNeeded']);

// ── Module-level lifecycle store ───────────────────────────────────
// App-lifetime, like the routing store: the pills keep their state
// across tab switches, and events that arrive while another tab is
// shown are not lost. Reset whenever the session changes.
let lifecycleMap: Record<string, AgentLifecycle> = {};
let lifecycleSession: string | null = null;
const lifecycleListeners = new Set<() => void>();
let lifecycleListenerStarted = false;

function emitLifecycleStore(): void {
  for (const l of lifecycleListeners) l();
}

/** Fold one backend lifecycle event into the store. */
export function applyLifecycleEvent(payload: LifecyclePayload): void {
  const phase = lifecyclePhase(payload.state, payload.transition);
  if (!phase) return;
  const owners = payload.owners?.length ? payload.owners : [payload.source];
  const next = { ...lifecycleMap };
  for (const agent of owners) next[agent] = phase;
  // A reviewer / the tester that judged someone else's work is done
  // with it, whatever the verdict.
  if (VERDICTS.has(payload.transition) && !owners.includes(payload.source)) {
    next[payload.source] = 'done';
  }
  lifecycleMap = next;
  emitLifecycleStore();
}

function ensureLifecycleListener(): void {
  if (lifecycleListenerStarted) return;
  lifecycleListenerStarted = true;
  listen<LifecyclePayload>('swarm-term:lifecycle', (event) => {
    applyLifecycleEvent(event.payload);
  }).catch((err) => {
    console.warn('[useAgentLifecycle] subscribe failed', err);
  });
}

function subscribeLifecycleStore(cb: () => void): () => void {
  lifecycleListeners.add(cb);
  return () => {
    lifecycleListeners.delete(cb);
  };
}

function lifecycleSnapshot(): Record<string, AgentLifecycle> {
  return lifecycleMap;
}

/** Test seam: forget every phase and the subscription. */
export function resetLifecycleStore(): void {
  lifecycleMap = {};
  lifecycleSession = null;
  lifecycleListenerStarted = false;
  emitLifecycleStore();
}

/** The backend lifecycle phases of `sessionId`'s agents. */
export function useLifecycleEvents(
  sessionId: string | null = null,
): Record<string, AgentLifecycle> {
  ensureLifecycleListener();
  useEffect(() => {
    if (sessionId !== lifecycleSession) {
      lifecycleSession = sessionId;
      lifecycleMap = {};
      emitLifecycleStore();
    }
  }, [sessionId]);
  return useSyncExternalStore(subscribeLifecycleStore, lifecycleSnapshot, lifecycleSnapshot);
}

/**
 * Per-agent lifecycle map keyed by agent id. The state machine is:
 *
 *   idle      — no `ok` route involving this agent in the last 60 s.
 *   assigned  — latest involvement is INBOUND (agent is `target` of a
 *               recent `ok` route) and the agent has not yet emitted
 *               its own routed reply.
 *   building  — latest involvement is OUTBOUND (agent is `source`)
 *               and the body does not match the done-prefix heuristic.
 *               Reviewers in this state are reported as `review`
 *               instead because their building IS review work.
 *   review    — agent IS a reviewer with any recent involvement, OR
 *               the most recent peer the agent talked to was a
 *               reviewer (so a builder waiting on review reads as
 *               `review`).
 *   done      — latest outbound body starts with a completion phrase
 *               (tamam / done / ✓ / completed).
 *
 * The map is sparse — agents never seen in the event stream simply
 * lack a key (consumers default to `idle`). Recomputed on every
 * `events` change; the work is O(events) ≤ O(500), fine for a UI.
 */
export function useAgentLifecycle(
  events: RouteEvent[],
  sessionId: string | null = null,
): Record<string, AgentLifecycle> {
  const authoritative = useLifecycleEvents(sessionId);
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), 5_000);
    return () => window.clearInterval(id);
  }, []);

  const heuristic = useMemo(() => {
    const out: Record<string, AgentLifecycle> = {};
    // events arrive newest-first, so the first match per agent IS the
    // most recent involvement — we can fix lifecycle and move on.
    const fixed = new Set<string>();
    for (const ev of events) {
      if (ev.outcome !== 'ok') continue;
      if (now - ev.ts > LIFECYCLE_WINDOW_MS) break; // events sorted desc by ts

      const considerAgent = (agentId: string, direction: 'in' | 'out') => {
        if (fixed.has(agentId)) return;
        const isReviewer = REVIEWER_AGENTS.has(agentId);
        let phase: AgentLifecycle;
        if (direction === 'in') {
          // Agent was just messaged. If sender is a reviewer the
          // agent (a builder) is in `review` rather than `assigned`.
          if (REVIEWER_AGENTS.has(ev.source) && !isReviewer) {
            phase = 'review';
          } else {
            phase = isReviewer ? 'review' : 'assigned';
          }
        } else {
          // Agent just emitted. Done if body starts with completion
          // phrase; otherwise building (or review, if agent is a
          // reviewer doing its job).
          if (DONE_BODY_PREFIX_RE.test(ev.body)) {
            phase = 'done';
          } else if (isReviewer) {
            phase = 'review';
          } else {
            phase = 'building';
          }
        }
        out[agentId] = phase;
        fixed.add(agentId);
      };

      considerAgent(ev.source, 'out');
      considerAgent(ev.target, 'in');
    }
    return out;
  }, [events, now]);

  // Authoritative backend lifecycle (swarm-term:lifecycle) wins over the
  // body-text heuristic wherever the store has emitted a state.
  return useMemo(
    () => ({ ...heuristic, ...authoritative }),
    [heuristic, authoritative],
  );
}

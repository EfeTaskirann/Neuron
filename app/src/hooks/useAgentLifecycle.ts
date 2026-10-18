import { useEffect, useMemo, useState } from 'react';
import { useTauriEvent } from './useTauriEvent';
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
  | 'done';

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
 */
interface LifecyclePayload {
  source: string;
  source_pane: string;
  task_id: string;
  transition: string;
  state: string;
}

const LIFECYCLE_STATE_TO_PHASE: Record<string, AgentLifecycle> = {
  Assigned: 'assigned',
  Building: 'building',
  AwaitingReview: 'review',
  Approved: 'done',
  Done: 'done',
  Failed: 'idle',
};

export function useLifecycleEvents(): Record<string, AgentLifecycle> {
  const [map, setMap] = useState<Record<string, AgentLifecycle>>({});

  useTauriEvent<LifecyclePayload>('swarm-term:lifecycle', (payload) => {
    const phase = LIFECYCLE_STATE_TO_PHASE[payload.state];
    if (!phase) return;
    setMap((prev) => ({ ...prev, [payload.source]: phase }));
  });

  return map;
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
): Record<string, AgentLifecycle> {
  const authoritative = useLifecycleEvents();
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

// `useAgentEvents(workspaceId, agentId)` — subscribes to one agent's
// per-(workspace, agent) Tauri event channel
// `swarm:agent:{ws}:{agent}:event` (WP-W4-03 §5).
//
// Returns a ring-buffered tail of recent events (cap 200) so the
// W4-04 grid pane can render a scrollable transcript without OOMing
// the renderer on a long-running session. Resubscribes when
// `workspaceId` or `agentId` changes — the W4-04 grid renders 9 panes
// from a fixed slot mapping, so in practice each hook instance keys
// to one (workspace, agent) for its lifetime.
//
// Side-channel pattern matches `useSwarmJob`'s W3-12c subscription:
// best-effort listen, no throw on listener registration failure
// (jsdom test runtime has no real Tauri bridge).
import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type { SwarmAgentEvent } from '../lib/bindings';

const TAIL_CAP = 200;

export function useAgentEvents(
  workspaceId: string,
  agentId: string,
): SwarmAgentEvent[] {
  const [events, setEvents] = useState<SwarmAgentEvent[]>([]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    const channel = `swarm:agent:${workspaceId}:${agentId}:event`;
    // Note: we deliberately do NOT call `setEvents([])` here on
    // (workspace, agent) change. Synchronous setState in an effect
    // triggers `react-hooks/set-state-in-effect` and causes a
    // cascading render. The W4-04 grid is expected to wrap each
    // pane in `key={workspaceId + agentId}` so React remounts the
    // component (and therefore re-initialises this hook's state to
    // `[]`) on key change — that's the reset path.
    listen<SwarmAgentEvent>(channel, (event) => {
      setEvents((prev) => {
        const next = [...prev, event.payload];
        return next.length > TAIL_CAP ? next.slice(-TAIL_CAP) : next;
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        // Listener registration is best-effort — Tauri rejects
        // when the runtime is not initialised (jsdom tests).
        console.warn('[useAgentEvents] failed to subscribe to', channel, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [workspaceId, agentId]);

  return events;
}

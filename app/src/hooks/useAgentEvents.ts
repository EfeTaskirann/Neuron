// `useAgentEvents(workspaceId, agentId)` — subscribes to one agent's
// per-(workspace, agent) Tauri event channel
// `swarm:agent:{ws}:{agent}:event` (WP-W4-03 §5).
//
// Returns a ring-buffered tail of recent events (cap 200) so the W4-04
// grid pane can render a scrollable transcript without OOMing the
// renderer on a long-running session. The shared `useTauriEvent`
// resubscribes when the channel (workspace/agent) changes.
//
// We deliberately do NOT reset `events` on (workspace, agent) change —
// the W4-04 grid wraps each pane in `key={ws + agent}` so React remounts
// the component (re-initialising this state to `[]`) on key change.
import { useState } from 'react';
import type { SwarmAgentEvent } from '../lib/bindings';
import { useTauriEvent } from './useTauriEvent';

const TAIL_CAP = 200;

export function useAgentEvents(
  workspaceId: string,
  agentId: string,
): SwarmAgentEvent[] {
  const [events, setEvents] = useState<SwarmAgentEvent[]>([]);
  useTauriEvent<SwarmAgentEvent>(
    `swarm:agent:${workspaceId}:${agentId}:event`,
    (payload) => {
      setEvents((prev) => {
        const next = [...prev, payload];
        return next.length > TAIL_CAP ? next.slice(-TAIL_CAP) : next;
      });
    },
  );
  return events;
}

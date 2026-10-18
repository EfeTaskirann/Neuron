import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

// Mock the Tauri event bridge so `swarm-term:lifecycle` can be fired.
type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: Handler) => {
    handlers.set(channel, handler);
    return vi.fn(() => {
      handlers.delete(channel);
    });
  }),
}));

import { useActiveEdge, useAgentLifecycle } from './useAgentLifecycle';
import type { RouteEvent, RouteOutcome } from './useRoutingEvents';

beforeEach(() => {
  handlers.clear();
});

let nextId = 0;
function ev(
  source: string,
  target: string,
  body: string,
  outcome: RouteOutcome = 'ok',
  ts = Date.now(),
): RouteEvent {
  return { id: nextId++, source, target, body, outcome, ts };
}

describe('useAgentLifecycle', () => {
  it('derives assigned / building / done from the newest ok routes', () => {
    // Newest-first, as the routing store delivers them.
    const events = [
      ev('frontend-builder', 'coordinator', 'tamam — header fixed'),
      ev('backend-builder', 'coordinator', 'BUILDING t1'),
      ev('coordinator', 'scout', 'map the auth module'),
    ];
    const { result } = renderHook(() => useAgentLifecycle(events));
    expect(result.current['frontend-builder']).toBe('done');
    expect(result.current['backend-builder']).toBe('building');
    expect(result.current.scout).toBe('assigned');
  });

  it('reports reviewers and builders messaged by a reviewer as review', () => {
    const events = [ev('backend-reviewer', 'backend-builder', 'CHANGES_NEEDED t1')];
    const { result } = renderHook(() => useAgentLifecycle(events));
    expect(result.current['backend-reviewer']).toBe('review');
    expect(result.current['backend-builder']).toBe('review');
  });

  it('ignores non-ok outcomes and routes outside the 60 s window', () => {
    const events = [
      ev('coordinator', 'planner', 'plan it', 'denied'),
      ev('coordinator', 'scout', 'old news', 'ok', Date.now() - 120_000),
    ];
    const { result } = renderHook(() => useAgentLifecycle(events));
    expect(result.current).toEqual({});
  });

  it('lets the backend lifecycle event override the heuristic', async () => {
    const events = [ev('backend-builder', 'coordinator', 'tamam — done')];
    const { result } = renderHook(() => useAgentLifecycle(events));
    expect(result.current['backend-builder']).toBe('done');
    await waitFor(() => expect(handlers.has('swarm-term:lifecycle')).toBe(true));
    act(() => {
      handlers.get('swarm-term:lifecycle')?.({
        payload: {
          source: 'backend-builder',
          source_pane: 'p1',
          task_id: 't1',
          transition: 'Done',
          state: 'AwaitingReview',
        },
      });
    });
    expect(result.current['backend-builder']).toBe('review');
  });
});

describe('useActiveEdge', () => {
  it('returns the newest ok route inside the ttl, skipping other outcomes', () => {
    const events = [
      ev('coordinator', 'nobody', 'x', 'unknown_target'),
      ev('orchestrator', 'coordinator', 'plan'),
    ];
    const { result } = renderHook(() => useActiveEdge(events, 3_000));
    expect(result.current).toMatchObject({
      source: 'orchestrator',
      target: 'coordinator',
    });
  });

  it('returns null once the newest ok route is older than the ttl', () => {
    const events = [ev('orchestrator', 'coordinator', 'plan', 'ok', Date.now() - 10_000)];
    const { result } = renderHook(() => useActiveEdge(events, 3_000));
    expect(result.current).toBeNull();
  });
});

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

import {
  applyLifecycleEvent,
  lifecyclePhase,
  resetLifecycleStore,
  useActiveEdge,
  useAgentLifecycle,
} from './useAgentLifecycle';
import type { RouteEvent, RouteOutcome } from './useRoutingEvents';

beforeEach(() => {
  handlers.clear();
  resetLifecycleStore();
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

describe('backend lifecycle events', () => {
  it('maps states to phases; a rejection is rework, a closed task failed', () => {
    expect(lifecyclePhase('Assigned', 'Assigned')).toBe('assigned');
    expect(lifecyclePhase('AwaitingReview', 'Done')).toBe('review');
    expect(lifecyclePhase('Approved', 'Approved')).toBe('done');
    expect(lifecyclePhase('Failed', 'ChangesNeeded')).toBe('rework');
    // Held for the user (a missing permission / install): not lost.
    expect(lifecyclePhase('Failed', 'Blocked')).toBe('rework');
    expect(lifecyclePhase('Failed', 'TaskFailed')).toBe('failed');
    expect(lifecyclePhase('Failed', 'Abandoned')).toBe('failed');
    expect(lifecyclePhase('Nonsense', 'x')).toBeNull();
  });

  it("puts a verdict on the builder's pill, and the reviewer is done", () => {
    const { result } = renderHook(() => useAgentLifecycle([], 's1'));
    act(() => {
      applyLifecycleEvent({
        source: 'backend-reviewer',
        source_pane: 'p-br',
        task_id: 't1',
        transition: 'ChangesNeeded',
        state: 'Failed',
        owners: ['backend-builder'],
      });
    });
    expect(result.current['backend-builder']).toBe('rework');
    expect(result.current['backend-reviewer']).toBe('done');
    act(() => {
      applyLifecycleEvent({
        source: 'coordinator',
        source_pane: 'p-c',
        task_id: 't1',
        transition: 'TaskFailed',
        state: 'Failed',
        owners: ['backend-builder'],
      });
    });
    expect(result.current['backend-builder']).toBe('failed');
    // The coordinator closed someone else's task: its own pill is untouched.
    expect(result.current.coordinator).toBeUndefined();
  });

  it('keeps the phases across remounts and forgets them for a new session', () => {
    const first = renderHook(() => useAgentLifecycle([], 's1'));
    act(() => {
      applyLifecycleEvent({
        source: 'backend-builder',
        source_pane: 'p-bb',
        task_id: 't1',
        transition: 'Building',
        state: 'Building',
      });
    });
    expect(first.result.current['backend-builder']).toBe('building');
    first.unmount();
    // Back on the tab: same session, same pills.
    const again = renderHook(() => useAgentLifecycle([], 's1'));
    expect(again.result.current['backend-builder']).toBe('building');
    again.unmount();
    const next = renderHook(() => useAgentLifecycle([], 's2'));
    expect(next.result.current['backend-builder']).toBeUndefined();
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

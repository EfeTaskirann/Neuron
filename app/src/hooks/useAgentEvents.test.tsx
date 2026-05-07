import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import type { SwarmAgentEvent } from '../lib/bindings';

// Mock the Tauri event listener so the hook can be driven without a
// real backend. The mock keeps a per-channel handler so the test can
// fire synthetic events.
type Handler = (event: { payload: SwarmAgentEvent }) => void;
const handlers = new Map<string, Handler>();
const unlistenSpies = new Map<string, () => void>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(
    async (channel: string, handler: Handler) => {
      handlers.set(channel, handler);
      const unlisten = vi.fn(() => {
        handlers.delete(channel);
      });
      unlistenSpies.set(channel, unlisten);
      return unlisten;
    },
  ),
}));

import { useAgentEvents } from './useAgentEvents';

beforeEach(() => {
  handlers.clear();
  unlistenSpies.clear();
});

function fire(channel: string, payload: SwarmAgentEvent): void {
  const h = handlers.get(channel);
  if (h) {
    h({ payload });
  }
}

describe('useAgentEvents', () => {
  it('collects events from the per-agent channel in order', async () => {
    const { result } = renderHook(() =>
      useAgentEvents('default', 'scout'),
    );
    // Wait for the listener to register.
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      fire('swarm:agent:default:scout:event', {
        kind: 'spawned',
        profile_id: 'scout',
      } as SwarmAgentEvent);
      fire('swarm:agent:default:scout:event', {
        kind: 'turn_started',
        turn_index: 0,
      } as SwarmAgentEvent);
      fire('swarm:agent:default:scout:event', {
        kind: 'assistant_text',
        delta: 'hello ',
      } as SwarmAgentEvent);
    });
    expect(result.current).toHaveLength(3);
    expect(result.current[0]?.kind).toBe('spawned');
    expect(result.current[1]?.kind).toBe('turn_started');
    expect(result.current[2]?.kind).toBe('assistant_text');
  });

  it('caps the buffer at 200 — overflow drops oldest events', async () => {
    const { result } = renderHook(() =>
      useAgentEvents('default', 'scout'),
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      for (let i = 0; i < 250; i += 1) {
        fire('swarm:agent:default:scout:event', {
          kind: 'assistant_text',
          delta: `chunk-${i}`,
        } as SwarmAgentEvent);
      }
    });
    expect(result.current).toHaveLength(200);
    // Oldest 50 dropped — the first event we now see is chunk-50.
    const first = result.current[0];
    expect(first?.kind).toBe('assistant_text');
    if (first?.kind === 'assistant_text') {
      expect(first.delta).toBe('chunk-50');
    }
    // Newest is chunk-249.
    const last = result.current[result.current.length - 1];
    if (last?.kind === 'assistant_text') {
      expect(last.delta).toBe('chunk-249');
    }
  });

  it('resubscribes when (workspaceId, agentId) changes — old channel unlistened', async () => {
    const { rerender } = renderHook(
      ({ ws, agent }: { ws: string; agent: string }) =>
        useAgentEvents(ws, agent),
      { initialProps: { ws: 'default', agent: 'scout' } },
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    const oldUnlisten = unlistenSpies.get(
      'swarm:agent:default:scout:event',
    );
    rerender({ ws: 'default', agent: 'planner' });
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:planner:event')).toBe(true),
    );
    // Old channel was unlistened.
    expect(oldUnlisten).toHaveBeenCalled();
    // Old handler removed.
    expect(handlers.has('swarm:agent:default:scout:event')).toBe(false);
  });
});

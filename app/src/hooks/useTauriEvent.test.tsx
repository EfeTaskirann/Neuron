import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

// Mock the Tauri event bridge: keep a per-channel handler + unlisten spy
// so tests can fire synthetic events and assert teardown.
type Handler = (event: { payload: unknown }) => void;
const handlers = new Map<string, Handler>();
const unlistenSpies = new Map<string, () => void>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: Handler) => {
    handlers.set(channel, handler);
    const unlisten = vi.fn(() => {
      handlers.delete(channel);
    });
    unlistenSpies.set(channel, unlisten);
    return unlisten;
  }),
}));

import { useTauriEvent } from './useTauriEvent';

beforeEach(() => {
  handlers.clear();
  unlistenSpies.clear();
});

function fire(channel: string, payload: unknown): void {
  handlers.get(channel)?.({ payload });
}

describe('useTauriEvent', () => {
  it('delivers event payloads to the handler in order', async () => {
    const seen: number[] = [];
    renderHook(() => useTauriEvent<number>('ch', (p) => seen.push(p)));
    await waitFor(() => expect(handlers.has('ch')).toBe(true));
    act(() => {
      fire('ch', 1);
      fire('ch', 2);
    });
    expect(seen).toEqual([1, 2]);
  });

  it('uses the latest handler without resubscribing on the same channel', async () => {
    const { rerender } = renderHook(
      ({ cb }: { cb: (p: number) => void }) => useTauriEvent<number>('ch', cb),
      { initialProps: { cb: (() => {}) as (p: number) => void } },
    );
    await waitFor(() => expect(handlers.has('ch')).toBe(true));
    const unlisten = unlistenSpies.get('ch');
    const seen: number[] = [];
    rerender({ cb: (p) => seen.push(p) });
    act(() => fire('ch', 42));
    expect(seen).toEqual([42]); // latest closure ran
    expect(unlisten).not.toHaveBeenCalled(); // no teardown → no resubscribe
  });

  it('unlistens on unmount', async () => {
    const { unmount } = renderHook(() => useTauriEvent('ch', () => {}));
    await waitFor(() => expect(handlers.has('ch')).toBe(true));
    const spy = unlistenSpies.get('ch');
    unmount();
    expect(spy).toHaveBeenCalled();
  });

  it('resubscribes when the channel changes', async () => {
    const { rerender } = renderHook(
      ({ ch }: { ch: string }) => useTauriEvent(ch, () => {}),
      { initialProps: { ch: 'a' } },
    );
    await waitFor(() => expect(handlers.has('a')).toBe(true));
    const oldUnlisten = unlistenSpies.get('a');
    rerender({ ch: 'b' });
    await waitFor(() => expect(handlers.has('b')).toBe(true));
    expect(oldUnlisten).toHaveBeenCalled();
  });

  it('skips subscribing when channel is null', () => {
    renderHook(() => useTauriEvent(null, () => {}));
    expect(handlers.size).toBe(0);
  });
});

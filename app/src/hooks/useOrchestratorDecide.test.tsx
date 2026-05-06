import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mock the bindings layer so the hook calls a controlled
// `swarmOrchestratorDecide`.
vi.mock('../lib/bindings', () => ({
  commands: {
    swarmOrchestratorDecide: vi.fn(),
  },
}));

import { useOrchestratorDecide } from './useOrchestratorDecide';
import type { OrchestratorOutcome } from '../lib/bindings';

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

const OUTCOME_OK: OrchestratorOutcome = {
  action: 'direct_reply',
  text: 'hello',
  reasoning: 'small talk',
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
    status: 'ok',
    data: OUTCOME_OK,
  });
});

describe('useOrchestratorDecide', () => {
  it('mutationFn invokes commands.swarmOrchestratorDecide and unwraps the outcome', async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useOrchestratorDecide(), {
      wrapper: wrapper(qc),
    });

    let returned: OrchestratorOutcome | undefined;
    await act(async () => {
      returned = await result.current.mutateAsync({
        workspaceId: 'default',
        userMessage: 'selam',
      });
    });

    const { commands } = await import('../lib/bindings');
    await waitFor(() =>
      expect(commands.swarmOrchestratorDecide).toHaveBeenCalledTimes(1),
    );
    const call = vi.mocked(commands.swarmOrchestratorDecide).mock.calls[0]!;
    expect(call[0]).toBe('default');
    expect(call[1]).toBe('selam');
    expect(returned).toEqual(OUTCOME_OK);
  });

  it('throws an Error with the backend message when the IPC errors', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'error',
      error: { kind: 'SwarmInvoke', message: 'persona did not parse' },
    });

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useOrchestratorDecide(), {
      wrapper: wrapper(qc),
    });

    await expect(
      result.current.mutateAsync({ workspaceId: 'default', userMessage: 'x' }),
    ).rejects.toThrow('persona did not parse');
  });
});

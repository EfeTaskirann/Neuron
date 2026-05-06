import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mock the bindings layer so the hook calls a controlled
// `swarmOrchestratorHistory`.
vi.mock('../lib/bindings', () => ({
  commands: {
    swarmOrchestratorHistory: vi.fn(),
  },
}));

import { useOrchestratorHistory } from './useOrchestratorHistory';
import type { OrchestratorMessage } from '../lib/bindings';

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

const HISTORY: OrchestratorMessage[] = [
  {
    id: 1,
    workspaceId: 'default',
    role: 'user',
    content: 'selam',
    goal: null,
    createdAtMs: 100,
  },
];

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmOrchestratorHistory).mockResolvedValue({
    status: 'ok',
    data: HISTORY,
  });
});

describe('useOrchestratorHistory', () => {
  it('calls swarmOrchestratorHistory with the workspaceId and unwraps the data', async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useOrchestratorHistory('default'), {
      wrapper: wrapper(qc),
    });
    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    const { commands } = await import('../lib/bindings');
    expect(commands.swarmOrchestratorHistory).toHaveBeenCalledWith(
      'default',
      null,
    );
    expect(result.current.data).toEqual(HISTORY);
  });
});

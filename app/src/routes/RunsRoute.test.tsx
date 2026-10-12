import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// T1-01 render-smoke for the `runs` tab. Mirrors the AgentsRoute /
// MCPRoute pattern: stub the `runs:list` command surface and drive the
// real `useRuns` hook through `unwrap`.
vi.mock('../lib/bindings', () => ({
  commands: {
    runsList: vi.fn(),
  },
}));

import { RunsRoute } from './RunsRoute';
import type { Run } from '../lib/bindings';

function renderRoute(): { qc: QueryClient } {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<RunsRoute />, { wrapper: Wrapper });
  return { qc };
}

const RUN: Run = {
  id: 'run-001',
  workflow: 'daily-summary',
  workflowId: 'wf-001',
  startedAt: 1_700_000_000,
  dur: 4200,
  tokens: 12_500,
  cost: 0.0123,
  status: 'success',
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.runsList).mockResolvedValue({ status: 'ok', data: [RUN] });
});

describe('RunsRoute', () => {
  it('shows the loading state before the runs query resolves', () => {
    renderRoute();
    expect(screen.getByText(/loading runs/i)).toBeInTheDocument();
  });

  it('renders a row per run once the query resolves', async () => {
    renderRoute();
    await waitFor(() => expect(screen.getByText('run-001')).toBeInTheDocument());
    expect(screen.getByText('daily-summary')).toBeInTheDocument();
  });

  it('renders the empty state when there are no runs', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.runsList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByTestId('runs-empty')).toBeInTheDocument(),
    );
    expect(screen.getByText(/henüz hiç çalışma yok/i)).toBeInTheDocument();
  });
});

import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mirror `SwarmRoute.test.tsx`: stub only the command surface the route
// reaches (agents list + the create/delete mutations its cards wire up),
// then drive the real `useAgents`/`useAgentCreate`/`useAgentDelete` hooks
// through `unwrap`. This is the T1-01 render-smoke coverage for the
// `agents` tab — it had none before.
vi.mock('../lib/bindings', () => ({
  commands: {
    agentsList: vi.fn(),
    agentsCreate: vi.fn(),
    agentsDelete: vi.fn(),
  },
}));

import { AgentsRoute } from './AgentsRoute';
import type { Agent } from '../lib/bindings';

function renderRoute(): { qc: QueryClient } {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<AgentsRoute />, { wrapper: Wrapper });
  return { qc };
}

const PLANNER: Agent = {
  id: 'agent-planner',
  name: 'Planner',
  model: 'gpt-4o',
  temp: 0.2,
  role: 'Plans the day',
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.agentsList).mockResolvedValue({ status: 'ok', data: [PLANNER] });
  vi.mocked(commands.agentsCreate).mockResolvedValue({ status: 'ok', data: PLANNER });
  vi.mocked(commands.agentsDelete).mockResolvedValue({ status: 'ok', data: null });
});

describe('AgentsRoute', () => {
  it('shows the loading state before the agents query resolves', () => {
    renderRoute();
    expect(screen.getByText(/loading agents/i)).toBeInTheDocument();
  });

  it('renders an agent card per agent once the query resolves', async () => {
    renderRoute();
    await waitFor(() => expect(screen.getByText('Planner')).toBeInTheDocument());
    expect(screen.getByText(/gpt-4o · temp 0\.2/)).toBeInTheDocument();
    expect(screen.getByText('Plans the day')).toBeInTheDocument();
    // The "+ New agent" affordance is the always-present trailing card.
    expect(screen.getByText('New agent')).toBeInTheDocument();
  });

  it('renders the empty grid (just the New agent button) with no agents', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.agentsList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() => expect(screen.getByText('New agent')).toBeInTheDocument());
    expect(screen.queryByText('Planner')).not.toBeInTheDocument();
  });

  it('duplicates an agent via agentsCreate with a "(copy)" name', async () => {
    const { commands } = await import('../lib/bindings');
    renderRoute();
    await waitFor(() => expect(screen.getByText('Planner')).toBeInTheDocument());
    fireEvent.click(screen.getByRole('button', { name: /duplicate/i }));
    await waitFor(() =>
      expect(commands.agentsCreate).toHaveBeenCalledWith({
        name: 'Planner (copy)',
        model: 'gpt-4o',
        temp: 0.2,
        role: 'Plans the day',
      }),
    );
  });
});

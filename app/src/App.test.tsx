import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClientProvider, QueryClient } from '@tanstack/react-query';
import { App } from './App';

// Mock the bindings layer so tests don't try to reach Tauri's
// `__TAURI_INVOKE`. Each test sets a happy-path default in
// `beforeEach`; specific cases override via mockResolvedValueOnce.
vi.mock('./lib/bindings', () => ({
  commands: {
    meGet: vi.fn(),
    agentsList: vi.fn(),
    runsList: vi.fn(),
    mcpList: vi.fn(),
    workflowsList: vi.fn(),
  },
}));

function renderApp(): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <App />
    </QueryClientProvider>,
  );
}

const ME_OK = {
  status: 'ok' as const,
  data: {
    user: { initials: 'EF', name: 'Efe' },
    workspace: { name: 'Personal', count: 3 },
  },
};

const AGENTS_OK = {
  status: 'ok' as const,
  data: [
    { id: 'a-1', name: 'Planner', model: 'gpt-4o', temp: 0.2, role: 'Plans the day' },
    { id: 'a-2', name: 'Coder', model: 'claude-3-5-sonnet', temp: 0.1, role: 'Writes code' },
  ],
};

const RUNS_OK = {
  status: 'ok' as const,
  data: [
    {
      id: 'r-1',
      workflow: 'Daily summary',
      workflowId: 'w-1',
      startedAt: Math.floor(Date.now() / 1000) - 120,
      dur: 2400,
      tokens: 1000,
      cost: 0.0123,
      status: 'success',
    },
    {
      id: 'r-2',
      workflow: 'Daily summary',
      workflowId: 'w-1',
      startedAt: Math.floor(Date.now() / 1000) - 30,
      dur: null,
      tokens: 500,
      cost: 0.005,
      status: 'running',
    },
  ],
};

beforeEach(async () => {
  const { commands } = await import('./lib/bindings');
  vi.mocked(commands.meGet).mockResolvedValue(ME_OK);
  vi.mocked(commands.agentsList).mockResolvedValue(AGENTS_OK);
  vi.mocked(commands.runsList).mockResolvedValue(RUNS_OK);
});

describe('App shell', () => {
  it('renders the sidebar with all 6 nav items', () => {
    renderApp();
    const nav = screen.getByRole('navigation');
    expect(nav).toBeInTheDocument();
    for (const label of ['Workflow', 'Terminal', 'Agents', 'Runs', 'MCP', 'Settings']) {
      expect(screen.getByText(label)).toBeInTheDocument();
    }
  });

  it('clicking a stub-only nav item swaps the active route stub', () => {
    renderApp();
    // MCP is still a stub in phase B/2; phase B/3 ports it. Clicking
    // it must surface the placeholder copy.
    fireEvent.click(screen.getByText('MCP'));
    const stub = screen.getByTestId('route-stub-mcp');
    expect(stub).toHaveTextContent(/mcp.*coming soon/i);
  });

  it('renders user and workspace from useMe()', async () => {
    renderApp();
    await waitFor(() => expect(screen.getByText('Efe')).toBeInTheDocument());
    expect(screen.getAllByText('Personal').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('3 workflows')).toBeInTheDocument();
    expect(screen.getAllByText('EF').length).toBeGreaterThanOrEqual(1);
  });
});

describe('AgentsRoute', () => {
  it('renders an agent card per backend agent + the New agent slot', async () => {
    renderApp();
    fireEvent.click(screen.getByText('Agents'));
    await waitFor(() => expect(screen.getByText('Planner')).toBeInTheDocument());
    expect(screen.getByText('Coder')).toBeInTheDocument();
    expect(screen.getByText(/gpt-4o.*temp.*0\.2/)).toBeInTheDocument();
    expect(screen.getByText('New agent')).toBeInTheDocument();
  });
});

describe('RunsRoute', () => {
  it('renders a row per run with derived totals', async () => {
    renderApp();
    fireEvent.click(screen.getByText('Runs'));
    await waitFor(() => expect(screen.getByText('r-1')).toBeInTheDocument());
    expect(screen.getByText('r-2')).toBeInTheDocument();
    // Totals across all runs (not the filtered view): tokens 1.5k,
    // cost 0.0173.
    expect(screen.getByText(/1\.5k/)).toBeInTheDocument();
    expect(screen.getByText(/\$0\.0173/)).toBeInTheDocument();
  });

  it('filters runs by status when a chip is clicked', async () => {
    renderApp();
    fireEvent.click(screen.getByText('Runs'));
    await waitFor(() => expect(screen.getByText('r-1')).toBeInTheDocument());
    // "running" appears as both a chip button and a status pill on
    // r-2's row; scope to button role to pick the chip.
    fireEvent.click(screen.getByRole('button', { name: /running/i }));
    expect(screen.queryByText('r-1')).not.toBeInTheDocument();
    expect(screen.getByText('r-2')).toBeInTheDocument();
  });
});

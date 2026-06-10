import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Render-smoke coverage for the `terminal-swarm` tab in isolation
// (T1-01 — App.test.tsx never reaches it). Mock at the command/event
// boundary like Terminal.test.tsx; drive the real hooks.
vi.mock('../lib/bindings', () => ({
  commands: {
    swarmTermListPersonas: vi.fn(),
    swarmTermSessionStatus: vi.fn(),
    swarmTermStartSession: vi.fn(),
    swarmTermStopSession: vi.fn(),
    swarmTermRunUpdate: vi.fn(),
    terminalLines: vi.fn(),
    terminalWrite: vi.fn(),
    terminalResize: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
}));

// xterm can't host in jsdom — same stub shape as App.test.tsx.
vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 80;
    rows = 24;
    loadAddon() {}
    open() {}
    write() {}
    clear() {}
    onData() {
      return { dispose: () => {} };
    }
    dispose() {}
  },
}));
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit() {}
  },
}));
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

import { TerminalSwarmRoute } from './TerminalSwarm';
import { setActiveProject } from '../hooks/useActiveProject';
import type { SwarmTermPersona, TerminalSwarmSessionHandle } from '../lib/bindings';

function renderRoute(): void {
  setActiveProject('C:\\test-project');
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<TerminalSwarmRoute />, { wrapper: Wrapper });
}

const PERSONAS: SwarmTermPersona[] = [
  {
    id: 'orchestrator',
    role: 'Orchestrator',
    description: 'routes user intent',
    allowedDestinations: ['coordinator'],
  },
  {
    id: 'coordinator',
    role: 'Coordinator',
    description: 'plans and dispatches',
    allowedDestinations: ['scout'],
  },
];

// sessionId carries a real ULID so parseSwarmTermStartMs can decode
// the launch timestamp for the session timer.
const SESSION: TerminalSwarmSessionHandle = {
  sessionId: 'swarm-term-01HV4Q2J9GXKWX8Z3M5T7BCDEF',
  projectDir: 'C:\\test-project',
  panes: [
    { agentId: 'orchestrator', paneId: 'p-orch' },
    { agentId: 'coordinator', paneId: 'p-coord' },
  ],
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmTermListPersonas).mockResolvedValue({
    status: 'ok',
    data: PERSONAS,
  });
  vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
    status: 'ok',
    data: null,
  });
  vi.mocked(commands.terminalLines).mockResolvedValue({ status: 'ok', data: [] });
  vi.mocked(commands.terminalWrite).mockResolvedValue({ status: 'ok', data: null });
  vi.mocked(commands.terminalResize).mockResolvedValue({ status: 'ok', data: null });
});

describe('TerminalSwarmRoute', () => {
  it('renders the idle grid with all 9 slots when no session is running', async () => {
    renderRoute();
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /launch swarm/i })).toBeInTheDocument(),
    );
    // 9 idle pane bodies, each carrying the launch hint.
    expect(
      screen.getAllByText(/idle — pick a project and launch/i),
    ).toHaveLength(9);
    // pane head + hierarchy chip both label the agent
    expect(screen.getAllByText('@orchestrator').length).toBeGreaterThanOrEqual(1);
    // No Stop button and no orchestrator chat while idle.
    expect(screen.queryByRole('button', { name: /stop swarm/i })).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText(/message to orchestrator/i),
    ).not.toBeInTheDocument();
  });

  it('renders the active-session chrome (Stop/Restart, chat, timer) when a session exists', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
      status: 'ok',
      data: SESSION,
    });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /stop swarm/i })).toBeInTheDocument(),
    );
    expect(screen.getByRole('button', { name: /restart/i })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /launch swarm/i })).not.toBeInTheDocument();
    // Orchestrator pane is live → the chat input mounts.
    expect(screen.getByLabelText(/message to orchestrator/i)).toBeInTheDocument();
    // Slots without a pane still show the idle hint (9 - 2 mounted).
    expect(
      screen.getAllByText(/idle — pick a project and launch/i),
    ).toHaveLength(7);
  });

  it('surfaces a session-status probe failure instead of silently showing the idle grid', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockRejectedValue(
      new Error('IPC bridge down'),
    );
    renderRoute();
    await waitFor(() =>
      expect(screen.getByText(/session status unavailable/i)).toBeInTheDocument(),
    );
    expect(screen.getByText(/IPC bridge down/i)).toBeInTheDocument();
  });
});

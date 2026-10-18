import { describe, expect, it, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
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
  permissionMode: 'restricted',
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

  it('states the restricted permission mode instead of offering a fake autonomy switch', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
      status: 'ok',
      data: SESSION,
    });
    renderRoute();
    const chip = await screen.findByText(/no approval prompts · role allowlists/i);
    expect(chip).toHaveAttribute(
      'title',
      expect.stringMatching(/builders can edit files inside this project/i),
    );
    // The old toggle changed nothing in the backend — it must stay gone.
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument();
    expect(screen.queryByText(/run autonomously/i)).not.toBeInTheDocument();
    expect(screen.queryByText(/permissions bypassed/i)).not.toBeInTheDocument();
  });

  it('flags a bypass-mode session as unrestricted', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
      status: 'ok',
      data: { ...SESSION, permissionMode: 'bypass' },
    });
    renderRoute();
    const chip = await screen.findByText(/permissions bypassed · unrestricted/i);
    expect(chip).toHaveClass('swarm-term-perm-chip--bypass');
    expect(chip).toHaveAttribute(
      'title',
      expect.stringMatching(/NEURON_TERM_PERMISSIONS=bypass/),
    );
    expect(screen.queryByText(/no approval prompts/i)).not.toBeInTheDocument();
  });

  it('sends a chat message as a bracketed paste followed by a separate CR', async () => {
    // Regression: `${message}\r` in one write left multi-line tasks
    // unsent in claude's input box (the CR was absorbed into the paste).
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
      status: 'ok',
      data: SESSION,
    });
    renderRoute();
    const input = await screen.findByLabelText(/message to orchestrator/i);
    fireEvent.change(input, { target: { value: 'refactor the bridge' } });
    fireEvent.submit(input.closest('form')!);

    await waitFor(() => {
      const orchWrites = vi
        .mocked(commands.terminalWrite)
        .mock.calls.filter(([paneId]) => paneId === 'p-orch')
        .map(([, data]) => data);
      expect(orchWrites).toEqual([
        '\x1b[200~refactor the bridge\x1b[201~',
        '\r',
      ]);
    });
  });

  it('strips control characters so a message cannot close the paste early', async () => {
    const { sanitizeForPaste } = await import('./TerminalSwarm');
    expect(sanitizeForPaste('a\x1b\x1b[201~\x03\x03b')).toBe('a[201~b');
    expect(sanitizeForPaste('ç\r\nş\rı\tğ\u007f\u009b')).toBe('ç\nş\nı\tğ');
  });

  it('serialises quick consecutive sends so paste/CR pairs never interleave', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermSessionStatus).mockResolvedValue({
      status: 'ok',
      data: SESSION,
    });
    renderRoute();
    const input = await screen.findByLabelText(/message to orchestrator/i);
    for (const text of ['first task', 'second task']) {
      fireEvent.change(input, { target: { value: text } });
      fireEvent.submit(input.closest('form')!);
    }

    await waitFor(
      () => {
        const orchWrites = vi
          .mocked(commands.terminalWrite)
          .mock.calls.filter(([paneId]) => paneId === 'p-orch')
          .map(([, data]) => data);
        expect(orchWrites).toEqual([
          '\x1b[200~first task\x1b[201~',
          '\r',
          '\x1b[200~second task\x1b[201~',
          '\r',
        ]);
      },
      { timeout: 2000 },
    );
  });

  it('says why a launch was refused, not just the generic error copy', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmTermStartSession).mockResolvedValue({
      status: 'error',
      error: { kind: 'invalid_input', message: '`C:\\` bir sürücü kökü.' },
    });
    renderRoute();
    const launch = await screen.findByRole('button', { name: /launch swarm/i });
    // Disabled until the session-status probe settles.
    await waitFor(() => expect(launch).toBeEnabled());
    fireEvent.click(launch);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Girilen değer geçersiz. `C:\\` bir sürücü kökü.');
  });

  it('describeError keeps the backend detail and handles plain errors', async () => {
    const { describeError } = await import('./TerminalSwarm');
    const { AppErrorClient } = await import('../lib/unwrap');
    expect(describeError(new AppErrorClient('internal', 'spawn agent `scout`: boom'))).toBe(
      'Beklenmedik bir iç hata oluştu. spawn agent `scout`: boom',
    );
    expect(describeError(new AppErrorClient('conflict', ''))).toBe(
      'İşlem mevcut bir kayıtla çakıştı.',
    );
    expect(describeError(new Error('IPC bridge down'))).toBe('IPC bridge down');
    expect(describeError('plain')).toBe('plain');
  });

  it('opens and closes the Routing Log drawer from the toolbar', async () => {
    renderRoute();
    const toggle = await screen.findByRole('button', { name: /routing log/i });
    expect(toggle).toHaveAttribute('aria-pressed', 'false');
    expect(screen.queryByRole('complementary', { name: /routing log/i })).not.toBeInTheDocument();

    fireEvent.click(toggle);
    const drawer = screen.getByRole('complementary', { name: /routing log/i });
    expect(drawer).toHaveTextContent(/no routing events yet/i);
    expect(toggle).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(toggle);
    expect(screen.queryByRole('complementary', { name: /routing log/i })).not.toBeInTheDocument();
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

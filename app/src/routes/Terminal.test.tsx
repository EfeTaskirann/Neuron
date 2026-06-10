import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mirror `AgentsRoute.test.tsx`: stub only the command surface the route
// reaches (pane list/scrollback + the spawn/kill/purge mutations its
// toolbar wires up), then drive the real `usePanes`/`usePaneLines`/
// `useMailbox` hooks through `unwrap`. Render-smoke coverage for the
// `terminal` tab in isolation — App.test.tsx only reaches it through
// the full shell.
vi.mock('../lib/bindings', () => ({
  commands: {
    terminalList: vi.fn(),
    terminalLines: vi.fn(),
    terminalSpawn: vi.fn(),
    terminalKill: vi.fn(),
    terminalWrite: vi.fn(),
    terminalResize: vi.fn(),
    terminalPurgeClosed: vi.fn(),
    terminalDelete: vi.fn(),
    mailboxList: vi.fn(),
  },
}));

// jsdom has no Tauri bridge; resolve to a no-op unsubscribe so the
// `.then(unlisten)` chains in usePaneLines/useMailbox/PaneBody settle.
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
}));

// xterm needs HTMLCanvasElement.getContext and window.matchMedia which
// jsdom doesn't ship. Stub the whole module (same shape as App.test.tsx)
// so PaneBody mounts as a no-op container — the xterm content itself
// isn't what we're testing in jsdom.
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
// xterm css import has no jsdom-relevant side-effect; map to empty.
vi.mock('@xterm/xterm/css/xterm.css', () => ({}));

import { TerminalRoute } from './Terminal';
import type { Pane } from '../lib/bindings';

function renderRoute(): { qc: QueryClient } {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<TerminalRoute />, { wrapper: Wrapper });
  return { qc };
}

const RUNNING_PANE: Pane = {
  id: 'p-claude',
  workspace: 'personal',
  agent: 'claude-code',
  role: 'builder',
  cwd: '~/work/neuron',
  status: 'running',
  pid: 4242,
  startedAt: Math.floor(Date.now() / 1000) - 60,
  closedAt: null,
  tokensIn: null,
  tokensOut: null,
  costUsd: null,
  uptime: null,
  approval: null,
};

const CLOSED_PANE: Pane = {
  id: 'p-shell',
  workspace: 'personal',
  agent: 'shell',
  role: null,
  cwd: '~',
  status: 'closed',
  pid: null,
  startedAt: Math.floor(Date.now() / 1000) - 600,
  closedAt: Math.floor(Date.now() / 1000) - 30,
  tokensIn: null,
  tokensOut: null,
  costUsd: null,
  uptime: null,
  approval: null,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.terminalList).mockResolvedValue({
    status: 'ok',
    data: [RUNNING_PANE, CLOSED_PANE],
  });
  vi.mocked(commands.terminalLines).mockResolvedValue({
    status: 'ok',
    data: [{ seq: 1, k: 'out', text: 'hello from the PTY' }],
  });
  vi.mocked(commands.mailboxList).mockResolvedValue({ status: 'ok', data: [] });
  vi.mocked(commands.terminalSpawn).mockResolvedValue({
    status: 'ok',
    data: RUNNING_PANE,
  });
  vi.mocked(commands.terminalKill).mockResolvedValue({ status: 'ok', data: null });
  vi.mocked(commands.terminalDelete).mockResolvedValue({ status: 'ok', data: null });
  vi.mocked(commands.terminalPurgeClosed).mockResolvedValue({
    status: 'ok',
    data: 1,
  });
  vi.mocked(commands.terminalWrite).mockResolvedValue({ status: 'ok', data: null });
  vi.mocked(commands.terminalResize).mockResolvedValue({ status: 'ok', data: null });
});

describe('TerminalRoute', () => {
  it('shows the loading state before the panes query resolves', () => {
    renderRoute();
    expect(screen.getByText(/loading panes/i)).toBeInTheDocument();
  });

  it('renders the tab strip, active pane, and status bar once panes resolve', async () => {
    renderRoute();
    // The active (first) pane's agent name renders twice: tab chip +
    // pane header. The closed shell pane only gets a tab chip because
    // a single pane body mounts at a time.
    await waitFor(() =>
      expect(screen.getAllByText('Claude').length).toBeGreaterThanOrEqual(2),
    );
    expect(screen.getByText('Shell')).toBeInTheDocument();
    expect(screen.getAllByRole('tab')).toHaveLength(2);
    expect(screen.getByRole('tab', { selected: true })).toHaveTextContent('Claude');
    // Status bar aggregates per-status counts.
    expect(screen.getByText(/2 panes/)).toBeInTheDocument();
    expect(screen.getByText(/1 running/)).toBeInTheDocument();
    expect(screen.getByText(/1 closed/)).toBeInTheDocument();
    // One closed pane → the bulk-cleanup button is armed with a count.
    expect(
      screen.getByRole('button', { name: /clean closed \(1\)/i }),
    ).not.toBeDisabled();
  });

  it('renders the empty state with the New pane button when no panes exist', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.terminalList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByText(/no panes yet/i)).toBeInTheDocument(),
    );
    expect(screen.getByRole('button', { name: /new pane/i })).toBeInTheDocument();
    expect(screen.queryByRole('tab')).not.toBeInTheDocument();
  });

  it('opens the spawn form and fires terminal:spawn with the typed cwd', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.terminalList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /new pane/i })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole('button', { name: /new pane/i }));
    const cwdInput = screen.getByLabelText(/working directory/i) as HTMLInputElement;
    fireEvent.change(cwdInput, { target: { value: 'C:/work/proj' } });
    // Submit the form directly — clicking a type=submit button doesn't
    // reliably propagate to onSubmit in jsdom (see App.test.tsx).
    fireEvent.submit(cwdInput.closest('form')!);
    await waitFor(() =>
      expect(commands.terminalSpawn).toHaveBeenCalledWith(
        expect.objectContaining({ cwd: 'C:/work/proj' }),
      ),
    );
  });
});

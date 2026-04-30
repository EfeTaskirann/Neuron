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
    runsGet: vi.fn(),
    mcpList: vi.fn(),
    workflowsList: vi.fn(),
    workflowsGet: vi.fn(),
    terminalList: vi.fn(),
    terminalLines: vi.fn(),
    mailboxList: vi.fn(),
    agentsCreate: vi.fn(),
    agentsDelete: vi.fn(),
    mcpInstall: vi.fn(),
    mcpUninstall: vi.fn(),
    runsCreate: vi.fn(),
  },
}));

// Tauri event listener is only meaningful inside the desktop app;
// in jsdom the native bridge is missing. The mock returns a real
// Promise resolving to a no-op unsubscribe so useRun's
// `.then(unlisten)` chain doesn't choke on undefined.
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
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

const WORKFLOW_OK = {
  status: 'ok' as const,
  data: {
    workflow: { id: 'daily-summary', name: 'Daily summary', savedAt: 1 },
    nodes: [
      {
        id: 'n1',
        workflowId: 'daily-summary',
        kind: 'llm',
        x: 60,
        y: 80,
        title: 'Planner',
        meta: 'gpt-4o · 1.2k tok',
        status: 'success',
      },
      {
        id: 'n2',
        workflowId: 'daily-summary',
        kind: 'tool',
        x: 360,
        y: 40,
        title: 'fetch_docs',
        meta: 'tool · 0.34s',
        status: 'success',
      },
    ],
    edges: [
      { id: 'e1', workflowId: 'daily-summary', fromNode: 'n1', toNode: 'n2', active: true },
    ],
  },
};

const RUN_DETAIL_OK = {
  status: 'ok' as const,
  data: {
    run: {
      id: 'r-1',
      workflow: 'Daily summary',
      workflowId: 'daily-summary',
      startedAt: Math.floor(Date.now() / 1000) - 120,
      dur: 2400,
      tokens: 3824,
      cost: 0.0124,
      status: 'success',
    },
    spans: [
      {
        id: 's-1',
        runId: 'r-1',
        parentSpanId: null,
        name: 'orchestrator.run',
        type: 'llm',
        t0Ms: 0,
        durationMs: 2400,
        attrsJson: '{"agent":"Reasoner","model":"gpt-4o"}',
        prompt: null,
        response: null,
        isRunning: false,
        indent: 0,
      },
      {
        id: 's-2',
        runId: 'r-1',
        parentSpanId: 's-1',
        name: 'llm.plan',
        type: 'llm',
        t0Ms: 40,
        durationMs: 680,
        attrsJson: '{"tokens_in":412,"tokens_out":168,"cost":0.0024}',
        prompt: 'Plan the day',
        response: 'Step 1, 2, 3',
        isRunning: false,
        indent: 1,
      },
    ],
  },
};

const SERVERS_OK = {
  status: 'ok' as const,
  data: [
    {
      id: 'filesystem',
      name: 'Filesystem',
      by: 'modelcontextprotocol',
      desc: 'Read/write files in a workspace root',
      installs: 12_400,
      rating: 4.8,
      featured: true,
      installed: true,
    },
    {
      id: 'github',
      name: 'GitHub',
      by: 'modelcontextprotocol',
      desc: 'Repo, issue, and PR access',
      installs: 8_900,
      rating: 4.6,
      featured: false,
      installed: false,
    },
  ],
};

beforeEach(async () => {
  const { commands } = await import('./lib/bindings');
  vi.mocked(commands.meGet).mockResolvedValue(ME_OK);
  vi.mocked(commands.agentsList).mockResolvedValue(AGENTS_OK);
  vi.mocked(commands.runsList).mockResolvedValue(RUNS_OK);
  vi.mocked(commands.mcpList).mockResolvedValue(SERVERS_OK);
  vi.mocked(commands.workflowsGet).mockResolvedValue(WORKFLOW_OK);
  vi.mocked(commands.runsGet).mockResolvedValue(RUN_DETAIL_OK);
  // Default: empty panes / empty scrollback so the terminal route
  // hits its empty-state branch unless a test overrides.
  vi.mocked(commands.terminalList).mockResolvedValue({ status: 'ok', data: [] });
  vi.mocked(commands.terminalLines).mockResolvedValue({ status: 'ok', data: [] });
  vi.mocked(commands.mailboxList).mockResolvedValue({ status: 'ok', data: [] });
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

  // No stub-only routes remain after phase D/1 — every nav item
  // dispatches to a real component. The previous "stub renders"
  // case is replaced by the per-route assertions below.

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

describe('MCPRoute', () => {
  it('renders featured + all-servers sections from useServers()', async () => {
    renderApp();
    fireEvent.click(screen.getByText('MCP'));
    await waitFor(() => expect(screen.getAllByText('Filesystem').length).toBeGreaterThan(0));
    // Filesystem (featured + installed) appears in featured AND all
    // sections; GitHub (not featured, not installed) only in all.
    expect(screen.getAllByText('Filesystem').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('GitHub')).toBeInTheDocument();
    // Installed pill renders for filesystem; install button renders
    // for github.
    expect(screen.getAllByText('installed').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('Install').length).toBeGreaterThanOrEqual(1);
  });
});

describe('Canvas', () => {
  it('renders nodes and edges from useWorkflow()', async () => {
    renderApp();
    // Canvas is the default route — content streams in once
    // workflowsGet resolves.
    await waitFor(() => expect(screen.getByText('Planner')).toBeInTheDocument());
    expect(screen.getByText('fetch_docs')).toBeInTheDocument();
    // Edge renders as <path> inside the canvas-edges svg; class
    // toggles `active` when active=true.
    const edges = document.querySelectorAll('.canvas-edge.active');
    expect(edges.length).toBe(1);
  });
});

describe('RunInspector', () => {
  // Inspector resolution is a 2-step async chain: useRuns settles
  // first (gives the most recent run), then useRun fires and
  // resolves the snapshot. `findByText` retries on async settle
  // and has a generous default — orchestrator.run also renders in
  // both the span-row and the sheet, so getAllByText is safer.
  it('renders span timeline from useRun()', async () => {
    renderApp();
    const orchestratorMatches = await screen.findAllByText(
      'orchestrator.run',
      {},
      { timeout: 5000 },
    );
    expect(orchestratorMatches.length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText('llm.plan')).toBeInTheDocument();
    expect(screen.getByText(/3,824 tokens/)).toBeInTheDocument();
    expect(screen.getByText(/\$0\.0124/)).toBeInTheDocument();
  });

  it('shows the prompt/response sheet for the selected llm span', async () => {
    renderApp();
    await screen.findByText('llm.plan', {}, { timeout: 5000 });
    fireEvent.click(screen.getByText('llm.plan'));
    expect(screen.getByText('Prompt')).toBeInTheDocument();
    expect(screen.getByText('Response')).toBeInTheDocument();
    expect(screen.getByText('Plan the day')).toBeInTheDocument();
  });
});

describe('TerminalRoute', () => {
  it('renders the empty state when terminal:list returns no panes', async () => {
    renderApp();
    fireEvent.click(screen.getByText('Terminal'));
    await waitFor(() =>
      expect(screen.getByText(/no panes yet/i)).toBeInTheDocument(),
    );
  });

  it('renders a pane card with header and scrollback lines', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.terminalList).mockResolvedValueOnce({
      status: 'ok',
      data: [
        {
          id: 'p-1',
          workspace: 'personal',
          agent: 'claude-code',
          role: null,
          cwd: '~/work/neuron',
          status: 'running',
          pid: 1234,
          startedAt: Math.floor(Date.now() / 1000) - 60,
          closedAt: null,
          tokensIn: null,
          tokensOut: null,
          costUsd: null,
          uptime: null,
          approval: null,
        },
      ],
    });
    vi.mocked(commands.terminalLines).mockResolvedValueOnce({
      status: 'ok',
      data: [
        { seq: 1, k: 'sys', text: 'session started' },
        { seq: 2, k: 'command', text: 'pnpm test' },
        { seq: 3, k: 'out', text: '✓ all tests passing' },
      ],
    });

    renderApp();
    fireEvent.click(screen.getByText('Terminal'));
    // Pane header arrives first (terminal:list); scrollback comes
    // through a separate query (terminal:lines) that resolves a
    // tick later. Wait for the lines specifically.
    await screen.findByText('Claude');
    expect(screen.getByText('~/work/neuron')).toBeInTheDocument();
    expect(screen.getByText(/^running$/)).toBeInTheDocument();
    await screen.findByText('session started');
    expect(screen.getByText('pnpm test')).toBeInTheDocument();
    expect(screen.getByText(/all tests passing/)).toBeInTheDocument();
    expect(screen.getByText(/1 panes/)).toBeInTheDocument();
    expect(screen.getByText(/1 running/)).toBeInTheDocument();
  });
});

describe('Mutations', () => {
  it('clicking + New agent reveals an inline form that calls agentsCreate', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.agentsCreate).mockResolvedValueOnce({
      status: 'ok',
      data: { id: 'a-3', name: 'Researcher', model: 'gpt-4o', temp: 0.4, role: 'Reads docs' },
    });

    renderApp();
    fireEvent.click(screen.getByText('Agents'));
    await waitFor(() => expect(screen.getByText('Planner')).toBeInTheDocument());
    // The "+ New agent" button is the only one with that text
    // before the form mounts. After click it gets replaced by the
    // form, where the same words appear inside <strong> — so we
    // look up the button explicitly.
    fireEvent.click(screen.getByRole('button', { name: /new agent/i }));
    // Find the form inputs by placeholder — `getAllByRole('textbox')`
    // also picks up the topbar search field, which would shift
    // indices and silently target the wrong input.
    const nameInput = screen.getByPlaceholderText('Planner') as HTMLInputElement;
    const roleInput = screen.getByPlaceholderText('Plans the day') as HTMLInputElement;
    fireEvent.change(nameInput, { target: { value: 'Researcher' } });
    fireEvent.change(roleInput, { target: { value: 'Reads docs' } });
    expect(nameInput.value).toBe('Researcher');
    // Submit the form directly — clicking a type=submit button
    // doesn't reliably propagate to onSubmit in jsdom.
    fireEvent.submit(nameInput.closest('form')!);
    await waitFor(() => expect(commands.agentsCreate).toHaveBeenCalled());
    const callArg = vi.mocked(commands.agentsCreate).mock.calls[0]![0];
    expect(callArg).toMatchObject({ name: 'Researcher', role: 'Reads docs' });
  });

  it('clicking Install on an MCP row calls mcpInstall and disables the button', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.mcpInstall).mockResolvedValueOnce({
      status: 'ok',
      data: { ...SERVERS_OK.data[1]!, installed: true },
    });

    renderApp();
    fireEvent.click(screen.getByText('MCP'));
    await waitFor(() => expect(screen.getByText('GitHub')).toBeInTheDocument());
    // Multiple Install buttons (featured + row); clicking any one
    // fires the mutation with the matching server id.
    const installButtons = screen.getAllByText('Install');
    fireEvent.click(installButtons[0]!);
    await waitFor(() => expect(commands.mcpInstall).toHaveBeenCalled());
    expect(vi.mocked(commands.mcpInstall).mock.calls[0]![0]).toBe('github');
  });

  it('topbar Run button on canvas calls runsCreate("daily-summary")', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.runsCreate).mockResolvedValueOnce({
      status: 'ok',
      data: {
        id: 'r-new',
        workflow: 'Daily summary',
        workflowId: 'daily-summary',
        startedAt: Math.floor(Date.now() / 1000),
        dur: null,
        tokens: 0,
        cost: 0,
        status: 'running',
      },
    });
    renderApp();
    // Default route is canvas; topbar Run button is visible.
    fireEvent.click(screen.getByRole('button', { name: /^Run$/i }));
    await waitFor(() => expect(commands.runsCreate).toHaveBeenCalled());
    expect(vi.mocked(commands.runsCreate).mock.calls[0]![0]).toBe('daily-summary');
  });
});

describe('Mailbox', () => {
  it('renders nothing when mailbox:list returns empty', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.terminalList).mockResolvedValueOnce({
      status: 'ok',
      data: [
        {
          id: 'p-x',
          workspace: 'personal',
          agent: 'shell',
          role: null,
          cwd: '~',
          status: 'idle',
          pid: null,
          startedAt: 1,
          closedAt: null,
          tokensIn: null,
          tokensOut: null,
          costUsd: null,
          uptime: null,
          approval: null,
        },
      ],
    });
    renderApp();
    fireEvent.click(screen.getByText('Terminal'));
    await screen.findByText('Shell'); // pane header arrives
    expect(screen.queryByText(/Mailbox/i)).not.toBeInTheDocument();
  });

  it('renders mailbox entries from useMailbox()', async () => {
    const { commands } = await import('./lib/bindings');
    vi.mocked(commands.terminalList).mockResolvedValueOnce({
      status: 'ok',
      data: [
        {
          id: 'p-x',
          workspace: 'personal',
          agent: 'shell',
          role: null,
          cwd: '~',
          status: 'idle',
          pid: null,
          startedAt: 1,
          closedAt: null,
          tokensIn: null,
          tokensOut: null,
          costUsd: null,
          uptime: null,
          approval: null,
        },
      ],
    });
    vi.mocked(commands.mailboxList).mockResolvedValueOnce({
      status: 'ok',
      data: [
        { id: 1, ts: Math.floor(Date.now() / 1000) - 30, from: 'planner', to: 'reasoner', type: 'task:done', summary: 'Plan complete' },
        { id: 2, ts: Math.floor(Date.now() / 1000) - 60, from: 'reasoner', to: 'human', type: 'await:approve', summary: 'Approve diff' },
      ],
    });
    renderApp();
    fireEvent.click(screen.getByText('Terminal'));
    await screen.findByText(/Mailbox · 2/);
    expect(screen.getByText('planner')).toBeInTheDocument();
    // 'reasoner' appears twice (to of entry-1, from of entry-2).
    expect(screen.getAllByText('reasoner').length).toBe(2);
    expect(screen.getByText('Plan complete')).toBeInTheDocument();
    expect(screen.getByText('Approve diff')).toBeInTheDocument();
  });
});

describe('SettingsRoute', () => {
  it('opens on Appearance and switches sections on click', () => {
    renderApp();
    fireEvent.click(screen.getByText('Settings'));
    // Appearance pane is the default — Theme/Accent rows render.
    expect(screen.getByText('Theme')).toBeInTheDocument();
    expect(screen.getByText('Accent')).toBeInTheDocument();
    // Click "Keys" — Appearance content should disappear and the
    // generic empty state for Keys should render.
    fireEvent.click(screen.getByText('Keys'));
    expect(screen.queryByText('Theme')).not.toBeInTheDocument();
    expect(screen.getByText('Settings for this section.')).toBeInTheDocument();
  });
});

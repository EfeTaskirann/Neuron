import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// H1 flow test: the Terminal Swarm the way the user meets it — launch →
// the panes get ready → a task goes to @orchestrator → the swarm reports
// done / failed → stop — through the real hooks, with the backend's
// commands mocked and its events replayed on a fake Tauri event bus.
// TerminalSwarm.test.tsx pins single states; this pins the transitions
// the events drive (the app-wide alerts, the routing and lifecycle
// stores, the toasts).

const bus = vi.hoisted(() => {
  type Handler = (event: { payload: unknown }) => void;
  const handlers = new Map<string, Set<Handler>>();
  return {
    listen(channel: string, handler: Handler) {
      const set = handlers.get(channel) ?? new Set<Handler>();
      handlers.set(channel, set);
      set.add(handler);
      return Promise.resolve(() => {
        set.delete(handler);
      });
    },
    emit(channel: string, payload: unknown) {
      for (const handler of [...(handlers.get(channel) ?? [])]) handler({ payload });
    },
  };
});

vi.mock('@tauri-apps/api/event', () => ({ listen: bus.listen }));

vi.mock('../lib/bindings', () => ({
  commands: {
    swarmTermListPersonas: vi.fn(),
    swarmTermSessionStatus: vi.fn(),
    swarmTermStartSession: vi.fn(),
    swarmTermTrustProject: vi.fn(),
    swarmTermStopSession: vi.fn(),
    swarmTermRestartAgent: vi.fn(),
    swarmTermResumeDeliveries: vi.fn(),
    swarmTermChanges: vi.fn(),
    swarmTermChangeDiff: vi.fn(),
    swarmTermRevertChanges: vi.fn(),
    swarmTermKeepChanges: vi.fn(),
    swarmTermSetPaused: vi.fn(),
    swarmTermExtendBudget: vi.fn(),
    swarmTermRunUpdate: vi.fn(),
    terminalLines: vi.fn(),
    terminalWrite: vi.fn(),
    terminalResize: vi.fn(),
  },
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({ ask: vi.fn() }));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ requestUserAttention: () => Promise.resolve() }),
  UserAttentionType: { Critical: 1, Informational: 2 },
}));

// xterm can't host in jsdom — same stub shape as TerminalSwarm.test.tsx.
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

import { commands } from '../lib/bindings';
import type {
  PaneReadiness,
  SwarmTermPersona,
  TerminalSwarmSessionHandle,
} from '../lib/bindings';
import { ask } from '@tauri-apps/plugin-dialog';
import { TerminalSwarmRoute } from './TerminalSwarm';
import { ToastHost } from '../components/ToastHost';
import { setActiveProject } from '../hooks/useActiveProject';
import { useSwarmTermAlerts } from '../hooks/useTerminalSwarmSession';
import { clearRoutingEvents } from '../hooks/useRoutingEvents';
import { resetLifecycleStore } from '../hooks/useAgentLifecycle';
import { dismissToast, subscribeToasts, type Toast } from '../lib/toast';

const AGENTS: Array<[string, string]> = [
  ['orchestrator', 'Orchestrator'],
  ['coordinator', 'Coordinator'],
  ['scout', 'Scout'],
  ['planner', 'Planner'],
  ['backend-builder', 'Backend Builder'],
  ['frontend-builder', 'Frontend Builder'],
  ['backend-reviewer', 'Backend Reviewer'],
  ['frontend-reviewer', 'Frontend Reviewer'],
  ['integration-tester', 'Integration Tester'],
];

const PERSONAS: SwarmTermPersona[] = AGENTS.map(([id, role]) => ({
  id,
  role,
  description: role,
  allowedDestinations: [],
}));

/** A fresh session of all nine agents, every pane still starting. */
function startedSession(): TerminalSwarmSessionHandle {
  return {
    // A real ULID, so the session timer can decode the launch time.
    sessionId: 'swarm-term-01HV4Q2J9GXKWX8Z3M5T7BCDEF',
    projectDir: 'C:\\test-project',
    panes: AGENTS.map(([agentId]) => ({ agentId, paneId: `p-${agentId}` })),
    permissionMode: 'restricted',
    claudeVersion: '2.1.296',
    preflightNote: null,
    paneStates: AGENTS.map(([agentId]) => ({ agentId, readiness: 'starting', detail: null })),
    usageLimited: false,
    limitResetsAt: null,
    changesNote: null,
    liteTeam: false,
    paused: false,
    messagesDelivered: 0,
    messageBudget: 300,
    budgetReached: false,
  };
}

function readySession(): TerminalSwarmSessionHandle {
  const s = startedSession();
  return { ...s, paneStates: s.paneStates.map((p) => ({ ...p, readiness: 'ready' })) };
}

/** What the backend holds: `swarmTermSessionStatus` answers with it. */
let session: TerminalSwarmSessionHandle | null = null;

/** The backend publishes a pane's readiness: the state, then the event. */
function paneState(agentId: string, readiness: PaneReadiness, detail: string | null = null): void {
  const s = session!;
  session = {
    ...s,
    paneStates: s.paneStates.map((p) => (p.agentId === agentId ? { ...p, readiness, detail } : p)),
  };
  act(() => bus.emit('swarm-term:pane-state', { sessionId: s.sessionId, agentId, readiness, detail }));
}

function route(source: string, target: string, body: string, outcome = 'ok'): void {
  act(() => bus.emit('swarm-term:route', { source, target, body, outcome }));
}

function lifecycle(source: string, taskId: string, transition: string, state: string, owners: string[]): void {
  act(() =>
    bus.emit('swarm-term:lifecycle', {
      source,
      source_pane: `p-${source}`,
      task_id: taskId,
      transition,
      state,
      owners,
    }),
  );
}

function orchestratorWrites(): string[] {
  return vi
    .mocked(commands.terminalWrite)
    .mock.calls.filter(([paneId]) => paneId === 'p-orchestrator')
    .map(([, data]) => data);
}

/** The route as the app mounts it: with the app-wide alerts and toasts. */
function renderFlow(): void {
  function Shell() {
    useSwarmTermAlerts();
    return (
      <>
        <TerminalSwarmRoute />
        <ToastHost />
      </>
    );
  }
  setActiveProject('C:\\test-project');
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <Shell />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  session = null;
  clearRoutingEvents();
  resetLifecycleStore();
  vi.mocked(commands.swarmTermListPersonas).mockResolvedValue({ status: 'ok', data: PERSONAS });
  vi.mocked(commands.swarmTermSessionStatus).mockImplementation(() =>
    Promise.resolve({ status: 'ok', data: session }),
  );
  vi.mocked(commands.swarmTermStartSession).mockImplementation(() => {
    session = startedSession();
    return Promise.resolve({ status: 'ok', data: session });
  });
  vi.mocked(commands.swarmTermStopSession).mockImplementation(() => {
    session = null;
    return Promise.resolve({ status: 'ok', data: null });
  });
  vi.mocked(commands.swarmTermRestartAgent).mockImplementation(() =>
    Promise.resolve({ status: 'ok', data: session! }),
  );
  vi.mocked(commands.swarmTermChanges).mockResolvedValue({
    status: 'ok',
    data: { checkpointAt: null, files: [], truncated: false, note: null },
  });
  vi.mocked(commands.terminalLines).mockResolvedValue({ status: 'ok', data: [] });
  vi.mocked(commands.terminalWrite).mockResolvedValue({ status: 'ok', data: null });
  vi.mocked(commands.terminalResize).mockResolvedValue({ status: 'ok', data: null });
});

afterEach(() => {
  // Toasts live in a module store and outlast a test's render; this runs
  // before the render is cleaned up, so the host still re-renders.
  let shown: readonly Toast[] = [];
  subscribeToasts((t) => {
    shown = t;
  })();
  act(() => {
    for (const t of shown) dismissToast(t.id);
  });
});

describe('Terminal Swarm flow', () => {
  it('takes a task from launch through ready to TASK_DONE', async () => {
    renderFlow();
    const launch = await screen.findByRole('button', { name: /launch swarm/i });
    await waitFor(() => expect(launch).toBeEnabled());
    fireEvent.click(launch);
    await waitFor(() => expect(commands.swarmTermStartSession).toHaveBeenCalledWith('C:\\test-project'));

    // Starting: the chat is there but locked, and says what it waits for.
    const input = await screen.findByLabelText(/message to orchestrator/i);
    expect(input).toBeDisabled();
    expect(input).toHaveAttribute('placeholder', 'Waiting for @orchestrator to start…');
    expect(screen.getByText('0/9 ready')).toBeInTheDocument();

    for (const [agent] of AGENTS) paneState(agent, 'kickoff_sent');
    await waitFor(() =>
      expect(input).toHaveAttribute('placeholder', 'Waiting for @orchestrator to answer its kickoff…'),
    );
    for (const [agent] of AGENTS) paneState(agent, 'ready');
    await waitFor(() => expect(input).toBeEnabled());
    expect(screen.getByText('9/9 ready')).toBeInTheDocument();

    // The task goes out as a bracketed paste, then its own CR.
    fireEvent.change(input, { target: { value: 'fix the bulk order bug' } });
    fireEvent.submit(input.closest('form')!);
    await waitFor(() =>
      expect(orchestratorWrites()).toEqual(['\x1b[200~fix the bulk order bug\x1b[201~', '\r']),
    );

    // The swarm works; the backend lifecycle moves the builder's pill.
    route('orchestrator', 'coordinator', 'T1: bulk orders start at 10');
    route('coordinator', 'backend-builder', 'T1: fix is_bulk_order');
    lifecycle('backend-builder', 'T1', 'Assigned', 'Assigned', ['backend-builder']);
    expect(await screen.findByLabelText('Lifecycle phase for backend-builder: assigned')).toBeInTheDocument();
    lifecycle('backend-builder', 'T1', 'Building', 'Building', ['backend-builder']);
    expect(await screen.findByLabelText('Lifecycle phase for backend-builder: building')).toBeInTheDocument();
    lifecycle('backend-builder', 'T1', 'Done', 'AwaitingReview', ['backend-builder']);
    expect(await screen.findByLabelText('Lifecycle phase for backend-builder: review')).toBeInTheDocument();
    lifecycle('backend-reviewer', 'T1', 'Approved', 'Approved', ['backend-builder']);
    expect(await screen.findByLabelText('Lifecycle phase for backend-builder: done')).toBeInTheDocument();
    expect(screen.getByLabelText('Lifecycle phase for backend-reviewer: done')).toBeInTheDocument();

    // The bridge's TASK_DONE reaches the orchestrator: the user is told.
    route('coordinator', 'orchestrator', 'TASK_DONE T1 — approved', 'lifecycle_fanout');
    expect(await screen.findByText('Görev tamamlandı: T1')).toBeInTheDocument();

    // The whole exchange is in the Routing Log.
    fireEvent.click(screen.getByRole('button', { name: /routing log/i }));
    const log = screen.getByRole('complementary', { name: /routing log/i });
    expect(log).toHaveTextContent('T1: fix is_bulk_order');
    expect(log).toHaveTextContent('TASK_DONE T1 — approved');
  });

  it('tells the user about a failed task, an exited agent and the usage limit', async () => {
    session = readySession();
    renderFlow();
    const input = await screen.findByLabelText(/message to orchestrator/i);
    await waitFor(() => expect(input).toBeEnabled());

    // A coordinator's TASK_FAILED is an ordinary route: still a failure toast.
    route('coordinator', 'orchestrator', 'TASK_FAILED S1 — cargo test fails in orders.rs');
    expect(await screen.findByText('Görev başarısız: S1 — cargo test fails in orders.rs')).toBeInTheDocument();
    // A message that is not a task signal tells nobody anything.
    route('coordinator', 'orchestrator', 'T2 dispatched to @frontend-builder');
    expect(screen.queryByText(/Görev .*T2/)).not.toBeInTheDocument();

    // An agent's claude exits: the badge, a restart on its pane, a toast.
    paneState('frontend-builder', 'exited', "Pane'in claude süreci kapandı.");
    expect(
      await screen.findByLabelText("frontend-builder: exited — Pane'in claude süreci kapandı."),
    ).toBeInTheDocument();
    expect(screen.getByText(/@frontend-builder kapandı/)).toBeInTheDocument();
    expect(screen.getByText('8/9 ready')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Restart Frontend Builder' }));
    await waitFor(() => expect(commands.swarmTermRestartAgent).toHaveBeenCalledWith('frontend-builder'));

    // The usage limit holds the swarm: a toast, the banner, the chat locked.
    session = { ...session!, usageLimited: true };
    act(() => bus.emit('swarm-term:session', { sessionId: session!.sessionId, state: 'limited', resetsAt: null }));
    expect(await screen.findByText(/^Claude kullanım limitine ulaşıldı; swarm duraklatıldı/)).toBeInTheDocument();
    await waitFor(() => expect(input).toBeDisabled());
    expect(input).toHaveAttribute('placeholder', expect.stringMatching(/^Kullanım limiti/));
    expect(screen.getByRole('button', { name: 'Şimdi sürdür' })).toBeInTheDocument();

    // claude gets through again: the hold lifts and the chat opens.
    session = { ...session!, usageLimited: false };
    act(() => bus.emit('swarm-term:session', { sessionId: session!.sessionId, state: 'running', resetsAt: null }));
    await waitFor(() => expect(input).toBeEnabled());
    expect(screen.queryByRole('button', { name: 'Şimdi sürdür' })).not.toBeInTheDocument();
  });

  it('stops only after the user confirms, and comes back to the idle grid', async () => {
    session = readySession();
    vi.mocked(ask).mockResolvedValueOnce(true);
    renderFlow();
    fireEvent.click(await screen.findByRole('button', { name: /stop swarm/i }));
    await waitFor(() => expect(commands.swarmTermStopSession).toHaveBeenCalled());
    expect(vi.mocked(ask).mock.calls[0]![0]).toContain('9 ajanın');
    expect(await screen.findAllByText(/idle — pick a project and launch/i)).toHaveLength(9);
    expect(screen.getByRole('button', { name: /launch swarm/i })).toBeInTheDocument();
    expect(screen.queryByLabelText(/message to orchestrator/i)).not.toBeInTheDocument();
  });
});

import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mock the bindings layer so the panel sees controlled
// `swarmOrchestratorDecide` and `swarmRunJob` calls. WP-W3-12k2
// added the persistent-history surface; the new IPC mocks default
// to "empty history / no-op writes" so existing tests keep
// asserting purely against the in-memory chat state.
vi.mock('../lib/bindings', () => ({
  commands: {
    swarmOrchestratorDecide: vi.fn(),
    swarmRunJob: vi.fn(),
    swarmOrchestratorHistory: vi.fn(),
    swarmOrchestratorClearHistory: vi.fn(),
    swarmOrchestratorLogJob: vi.fn(),
  },
}));

import { OrchestratorChatPanel } from './OrchestratorChatPanel';
import type { JobOutcome, OrchestratorOutcome } from '../lib/bindings';

function renderPanel(onSelectJob: (id: string) => void = () => {}): {
  qc: QueryClient;
} {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(
    <OrchestratorChatPanel workspaceId="default" onSelectJob={onSelectJob} />,
    { wrapper: Wrapper },
  );
  return { qc };
}

const DIRECT_REPLY: OrchestratorOutcome = {
  action: 'direct_reply',
  text: 'Hello! How can I help?',
  reasoning: 'greeting',
};

const CLARIFY: OrchestratorOutcome = {
  action: 'clarify',
  text: 'Which file should I refactor?',
  reasoning: 'goal too ambiguous',
};

const DISPATCH: OrchestratorOutcome = {
  action: 'dispatch',
  text: 'Add a JSDoc header to src/lib/foo.ts',
  reasoning: 'concrete enough',
};

const JOB_OK: JobOutcome = {
  jobId: 'a-1234abcd5678',
  finalState: 'done',
  stages: [],
  lastError: null,
  totalCostUsd: 0,
  totalDurationMs: 0,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmOrchestratorDecide).mockReset();
  vi.mocked(commands.swarmRunJob).mockReset();
  vi.mocked(commands.swarmOrchestratorHistory).mockReset();
  vi.mocked(commands.swarmOrchestratorClearHistory).mockReset();
  vi.mocked(commands.swarmOrchestratorLogJob).mockReset();
  vi.mocked(commands.swarmRunJob).mockResolvedValue({
    status: 'ok',
    data: JOB_OK,
  });
  vi.mocked(commands.swarmOrchestratorHistory).mockResolvedValue({
    status: 'ok',
    data: [],
  });
  vi.mocked(commands.swarmOrchestratorClearHistory).mockResolvedValue({
    status: 'ok',
    data: null,
  });
  vi.mocked(commands.swarmOrchestratorLogJob).mockResolvedValue({
    status: 'ok',
    data: null,
  });
});

async function typeAndSend(text: string): Promise<void> {
  const textarea = screen.getByPlaceholderText(/type a message/i) as HTMLTextAreaElement;
  fireEvent.change(textarea, { target: { value: text } });
  const sendBtn = screen.getByRole('button', { name: /send/i });
  await act(async () => {
    fireEvent.click(sendBtn);
  });
}

describe('OrchestratorChatPanel', () => {
  it('renders empty state with explainer when there are no messages', () => {
    renderPanel();
    expect(
      screen.getByText(/chat with the swarm orchestrator/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/ask questions or describe what you want to build/i),
    ).toBeInTheDocument();
  });

  it('appends user message on submit', async () => {
    const { commands } = await import('../lib/bindings');
    // Hold the decide promise open so the user bubble lands
    // before the orchestrator bubble.
    vi.mocked(commands.swarmOrchestratorDecide).mockImplementation(
      () => new Promise(() => {}),
    );
    renderPanel();
    await typeAndSend('hello there');
    expect(screen.getByText('hello there')).toBeInTheDocument();
    expect(commands.swarmOrchestratorDecide).toHaveBeenCalledWith(
      'default',
      'hello there',
    );
  });

  it('renders orchestrator direct_reply outcome as a bot bubble', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'ok',
      data: DIRECT_REPLY,
    });
    renderPanel();
    await typeAndSend('selam');
    await waitFor(() =>
      expect(screen.getByText('Hello! How can I help?')).toBeInTheDocument(),
    );
    expect(commands.swarmRunJob).not.toHaveBeenCalled();
  });

  it('renders orchestrator clarify outcome as a bot bubble', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'ok',
      data: CLARIFY,
    });
    renderPanel();
    await typeAndSend('Auth refactor yap');
    await waitFor(() =>
      expect(
        screen.getByText('Which file should I refactor?'),
      ).toBeInTheDocument(),
    );
    expect(commands.swarmRunJob).not.toHaveBeenCalled();
  });

  it('dispatch outcome triggers run_job and appends a job message', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'ok',
      data: DISPATCH,
    });
    renderPanel();
    await typeAndSend('EXECUTE: doc the thing');
    await waitFor(() => expect(commands.swarmRunJob).toHaveBeenCalledTimes(1));
    const call = vi.mocked(commands.swarmRunJob).mock.calls[0]!;
    expect(call[0]).toBe('default');
    expect(call[1]).toBe(DISPATCH.text);
    // Job pill renders the truncated jobId.
    await waitFor(() =>
      expect(screen.getByText(/Started job/i)).toBeInTheDocument(),
    );
    expect(
      screen.getByRole('button', { name: JOB_OK.jobId.slice(0, 8) }),
    ).toBeInTheDocument();
  });

  // Regression: a mid-session history invalidate would refetch the
  // freshly persisted user/orchestrator/job rows while the panel's
  // localMessages still held the same three bubbles, doubling each
  // bubble on screen. The hook layer must NOT invalidate
  // `['orchestrator-history']` from the dispatch chain — the panel
  // relies on its mount-time fetch to pick up persistence on the
  // next mount only.
  it('dispatch flow does not duplicate bubbles even if the next history fetch would return the persisted thread', async () => {
    const { commands } = await import('../lib/bindings');
    // First fetch: empty (initial mount). Subsequent fetches: the
    // would-be persisted thread. If the hook layer ever invalidates,
    // the panel would refetch and double up — the regression guard
    // counts each unique text exactly once.
    const persistedThread = [
      {
        id: 1,
        workspaceId: 'default',
        role: 'user' as const,
        content: 'EXECUTE: doc the thing',
        goal: null,
        createdAtMs: 100,
      },
      {
        id: 2,
        workspaceId: 'default',
        role: 'orchestrator' as const,
        content: JSON.stringify(DISPATCH),
        goal: null,
        createdAtMs: 200,
      },
      {
        id: 3,
        workspaceId: 'default',
        role: 'job' as const,
        content: JOB_OK.jobId,
        goal: DISPATCH.text,
        createdAtMs: 300,
      },
    ];
    vi.mocked(commands.swarmOrchestratorHistory)
      .mockResolvedValueOnce({ status: 'ok', data: [] })
      .mockResolvedValue({ status: 'ok', data: persistedThread });
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'ok',
      data: DISPATCH,
    });
    renderPanel();
    await typeAndSend('EXECUTE: doc the thing');
    await waitFor(() =>
      expect(commands.swarmOrchestratorLogJob).toHaveBeenCalledTimes(1),
    );
    // User bubble appears exactly once. (DISPATCH.text shows in
    // both the orchestrator bubble *and* the job bubble's goal
    // preview, so it isn't a unique-count check; the user-typed
    // string and the job link are the clean duplicate canaries.)
    await waitFor(() =>
      expect(screen.getAllByText('EXECUTE: doc the thing')).toHaveLength(1),
    );
    // Job link button appears exactly once.
    expect(
      screen.getAllByRole('button', { name: JOB_OK.jobId.slice(0, 8) }),
    ).toHaveLength(1);
  });

  it('shows an error banner when orchestrator_decide rejects', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'error',
      error: { kind: 'SwarmInvoke', message: 'persona blew up' },
    });
    renderPanel();
    await typeAndSend('boom');
    await waitFor(() =>
      expect(screen.getByRole('alert')).toHaveTextContent('persona blew up'),
    );
  });

  it('clicking the job message calls onSelectJob with the jobId', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
      status: 'ok',
      data: DISPATCH,
    });
    const onSelectJob = vi.fn();
    renderPanel(onSelectJob);
    await typeAndSend('EXECUTE: x');
    const link = await screen.findByRole('button', {
      name: JOB_OK.jobId.slice(0, 8),
    });
    fireEvent.click(link);
    expect(onSelectJob).toHaveBeenCalledWith(JOB_OK.jobId);
  });

  it('disables the Send button while a decision is in flight', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorDecide).mockImplementation(
      () => new Promise(() => {}),
    );
    renderPanel();
    await typeAndSend('hold the line');
    const sendBtn = screen.getByRole('button', { name: /sending/i });
    expect(sendBtn).toBeDisabled();
    const textarea = screen.getByPlaceholderText(
      /type a message/i,
    ) as HTMLTextAreaElement;
    expect(textarea).toBeDisabled();
  });

  // ------------------------------------------------------------ //
  // WP-W3-12k2 — persistent history seed + clear button          //
  // ------------------------------------------------------------ //

  it('seeds messages from history on mount', async () => {
    const { commands } = await import('../lib/bindings');
    // Seed a tiny thread: user msg → orchestrator outcome → job row.
    vi.mocked(commands.swarmOrchestratorHistory).mockResolvedValue({
      status: 'ok',
      data: [
        {
          id: 1,
          workspaceId: 'default',
          role: 'user',
          content: 'önceki mesaj',
          goal: null,
          createdAtMs: 100,
        },
        {
          id: 2,
          workspaceId: 'default',
          role: 'orchestrator',
          content: JSON.stringify({
            action: 'direct_reply',
            text: 'önceki cevap',
            reasoning: 'r',
          }),
          goal: null,
          createdAtMs: 200,
        },
        {
          id: 3,
          workspaceId: 'default',
          role: 'job',
          content: 'a-1234abcd5678',
          goal: 'önceki goal',
          createdAtMs: 300,
        },
      ],
    });
    renderPanel();
    // User bubble.
    await waitFor(() =>
      expect(screen.getByText('önceki mesaj')).toBeInTheDocument(),
    );
    // Orchestrator bubble.
    expect(screen.getByText('önceki cevap')).toBeInTheDocument();
    // Job bubble.
    expect(
      screen.getByRole('button', { name: 'a-1234ab' }),
    ).toBeInTheDocument();
    expect(commands.swarmOrchestratorHistory).toHaveBeenCalledWith(
      'default',
      null,
    );
  });

  it('clear chat button clears local messages and invalidates history', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.swarmOrchestratorHistory).mockResolvedValue({
      status: 'ok',
      data: [
        {
          id: 1,
          workspaceId: 'default',
          role: 'user',
          content: 'temizlenecek',
          goal: null,
          createdAtMs: 100,
        },
      ],
    });
    const { qc } = renderPanel();
    await waitFor(() =>
      expect(screen.getByText('temizlenecek')).toBeInTheDocument(),
    );
    const invalidateSpy = vi.spyOn(qc, 'invalidateQueries');
    const clearBtn = screen.getByRole('button', { name: /clear chat/i });
    expect(clearBtn).not.toBeDisabled();
    await act(async () => {
      fireEvent.click(clearBtn);
    });
    await waitFor(() =>
      expect(commands.swarmOrchestratorClearHistory).toHaveBeenCalledWith(
        'default',
      ),
    );
    // Local bubble removed.
    await waitFor(() =>
      expect(screen.queryByText('temizlenecek')).not.toBeInTheDocument(),
    );
    expect(invalidateSpy).toHaveBeenCalledWith({
      queryKey: ['orchestrator-history', 'default'],
    });
  });
});

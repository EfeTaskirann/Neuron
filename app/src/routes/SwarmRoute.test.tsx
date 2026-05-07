import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

vi.mock('../lib/bindings', () => ({
  commands: {
    swarmListJobs: vi.fn(),
    swarmGetJob: vi.fn(),
    swarmRunJob: vi.fn(),
    swarmCancelJob: vi.fn(),
    swarmOrchestratorDecide: vi.fn(),
    swarmOrchestratorHistory: vi.fn(),
    swarmOrchestratorClearHistory: vi.fn(),
    swarmOrchestratorLogJob: vi.fn(),
    swarmAgentsListStatus: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
}));

import { SwarmRoute } from './SwarmRoute';
import type { JobDetail, JobSummary } from '../lib/bindings';

function renderRoute(): { qc: QueryClient } {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<SwarmRoute />, { wrapper: Wrapper });
  return { qc };
}

const RUNNING_JOB: JobSummary = {
  id: 'job-running',
  workspaceId: 'default',
  goal: 'do the running thing',
  createdAtMs: Date.now() - 60_000,
  finishedAtMs: null,
  state: 'build',
  stageCount: 2,
  totalCostUsd: 0.02,
  lastError: null,
};

const FAILED_JOB: JobSummary = {
  id: 'job-failed',
  workspaceId: 'default',
  goal: 'this one tipped over',
  createdAtMs: Date.now() - 120_000,
  finishedAtMs: Date.now() - 60_000,
  state: 'failed',
  stageCount: 1,
  totalCostUsd: 0.01,
  lastError: 'boom',
};

const RUNNING_DETAIL: JobDetail = {
  id: 'job-running',
  workspaceId: 'default',
  goal: 'do the running thing',
  createdAtMs: Date.now() - 60_000,
  finishedAtMs: null,
  state: 'build',
  retryCount: 0,
  stages: [],
  lastError: null,
  totalCostUsd: 0.02,
  totalDurationMs: 5000,
};

const FAILED_DETAIL: JobDetail = {
  id: 'job-failed',
  workspaceId: 'default',
  goal: 'this one tipped over',
  createdAtMs: Date.now() - 120_000,
  finishedAtMs: Date.now() - 60_000,
  state: 'failed',
  retryCount: 0,
  stages: [],
  lastError: 'boom',
  totalCostUsd: 0.01,
  totalDurationMs: 1000,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmListJobs).mockResolvedValue({
    status: 'ok',
    data: [RUNNING_JOB, FAILED_JOB],
  });
  vi.mocked(commands.swarmGetJob).mockImplementation(async (id) => {
    if (id === 'job-running') return { status: 'ok', data: RUNNING_DETAIL };
    if (id === 'job-failed') return { status: 'ok', data: FAILED_DETAIL };
    return { status: 'error', error: { kind: 'NotFound', message: 'no such job' } };
  });
  vi.mocked(commands.swarmRunJob).mockResolvedValue({
    status: 'ok',
    data: {
      jobId: 'job-new',
      finalState: 'done',
      stages: [],
      lastError: null,
      totalCostUsd: 0,
      totalDurationMs: 0,
    },
  });
  vi.mocked(commands.swarmCancelJob).mockResolvedValue({
    status: 'ok',
    data: null,
  });
  vi.mocked(commands.swarmOrchestratorDecide).mockResolvedValue({
    status: 'ok',
    data: {
      action: 'dispatch',
      text: 'ship a feature',
      reasoning: 'concrete',
    },
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
  vi.mocked(commands.swarmAgentsListStatus).mockResolvedValue({
    status: 'ok',
    data: [],
  });
});

/**
 * W4-04 changed `SwarmRoute`'s default view to the 3×3 grid; the
 * legacy chat-shaped view (jobs + chat panel + detail) is now
 * gated behind a "Recent jobs" tab. These existing tests assert
 * against the legacy view, so they need to switch the tab first.
 */
function switchToJobsView(): void {
  fireEvent.click(screen.getByRole('button', { name: /recent jobs/i }));
}

describe('SwarmRoute', () => {
  it('renders the empty-state on the right pane until a job is selected', async () => {
    renderRoute();
    switchToJobsView();
    expect(screen.getByText(/select a job from the left/i)).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByText('do the running thing')).toBeInTheDocument(),
    );
  });

  it('renders the OrchestratorChatPanel as the left-pane top section', async () => {
    renderRoute();
    switchToJobsView();
    expect(
      screen.getByPlaceholderText(/type a message/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/chat with the swarm orchestrator/i),
    ).toBeInTheDocument();
  });

  it('chat dispatch outcome fires swarmRunJob with the refined goal', async () => {
    renderRoute();
    switchToJobsView();
    const { commands } = await import('../lib/bindings');
    const textarea = screen.getByPlaceholderText(/type a message/i) as HTMLTextAreaElement;
    fireEvent.change(textarea, { target: { value: 'EXECUTE: ship a feature' } });
    fireEvent.click(screen.getByRole('button', { name: /send/i }));
    await waitFor(() =>
      expect(commands.swarmOrchestratorDecide).toHaveBeenCalled(),
    );
    const decideCall = vi.mocked(commands.swarmOrchestratorDecide).mock.calls[0]!;
    expect(decideCall[0]).toBe('default');
    expect(decideCall[1]).toBe('EXECUTE: ship a feature');
    await waitFor(() => expect(commands.swarmRunJob).toHaveBeenCalled());
    const runCall = vi.mocked(commands.swarmRunJob).mock.calls[0]!;
    expect(runCall[0]).toBe('default');
    // Refined goal returned from the mocked orchestrator decision.
    expect(runCall[1]).toBe('ship a feature');
  });

  it('clicking a job row populates the detail pane and shows Cancel for non-terminal jobs', async () => {
    renderRoute();
    switchToJobsView();
    await waitFor(() =>
      expect(screen.getByText('do the running thing')).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByText('do the running thing'));
    // Detail pane shows the goal, which appears twice now (list +
    // detail). Cancel button visible because state=build is non-
    // terminal.
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /cancel/i })).toBeInTheDocument(),
    );
    // No Rerun on a still-running job.
    expect(screen.queryByRole('button', { name: /rerun/i })).not.toBeInTheDocument();
  });

  it('shows Rerun (and hides Cancel) when a Failed job is selected', async () => {
    renderRoute();
    switchToJobsView();
    await waitFor(() =>
      expect(screen.getByText('this one tipped over')).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByText('this one tipped over'));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /rerun/i })).toBeInTheDocument(),
    );
    expect(screen.queryByRole('button', { name: /^cancel$/i })).not.toBeInTheDocument();
  });

  it('clicking Cancel on a running job fires swarmCancelJob with the job id', async () => {
    renderRoute();
    switchToJobsView();
    const { commands } = await import('../lib/bindings');
    await waitFor(() =>
      expect(screen.getByText('do the running thing')).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByText('do the running thing'));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: /cancel/i })).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole('button', { name: /cancel/i }));
    await waitFor(() => expect(commands.swarmCancelJob).toHaveBeenCalled());
    expect(vi.mocked(commands.swarmCancelJob).mock.calls[0]![0]).toBe('job-running');
  });
});

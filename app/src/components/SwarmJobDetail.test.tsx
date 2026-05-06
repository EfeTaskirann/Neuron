import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

vi.mock('../lib/bindings', () => ({
  commands: {
    swarmGetJob: vi.fn(),
    swarmRunJob: vi.fn(),
    swarmCancelJob: vi.fn(),
  },
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
}));

import { SwarmJobDetail } from './SwarmJobDetail';
import type { JobDetail } from '../lib/bindings';

function renderDetail(jobId: string | null) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  return render(
    <SwarmJobDetail jobId={jobId} workspaceId="default" />,
    { wrapper: Wrapper },
  );
}

const DONE_DETAIL: JobDetail = {
  id: 'job-done',
  workspaceId: 'default',
  goal: 'a finished goal',
  createdAtMs: Date.now() - 120_000,
  finishedAtMs: Date.now() - 60_000,
  state: 'done',
  retryCount: 0,
  stages: [
    {
      state: 'scout',
      specialistId: 'scout',
      assistantText: 'scout result text',
      sessionId: 'sess-1',
      totalCostUsd: 0.01,
      durationMs: 1000,
    },
    {
      state: 'plan',
      specialistId: 'planner',
      assistantText: 'plan result text',
      sessionId: 'sess-2',
      totalCostUsd: 0.02,
      durationMs: 2000,
    },
    {
      state: 'build',
      specialistId: 'backend-builder',
      assistantText: 'build result text',
      sessionId: 'sess-3',
      totalCostUsd: 0.03,
      durationMs: 3000,
    },
  ],
  lastError: null,
  totalCostUsd: 0.06,
  totalDurationMs: 6000,
};

const FAILED_DETAIL: JobDetail = {
  ...DONE_DETAIL,
  id: 'job-failed',
  state: 'failed',
  lastError: 'something went wrong',
  goal: 'a failed goal',
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmGetJob).mockImplementation(async (id) => {
    if (id === 'job-done') return { status: 'ok', data: DONE_DETAIL };
    if (id === 'job-failed') return { status: 'ok', data: FAILED_DETAIL };
    return { status: 'error', error: { kind: 'NotFound', message: 'nope' } };
  });
});

describe('SwarmJobDetail', () => {
  it('renders empty state when jobId is null', () => {
    renderDetail(null);
    expect(screen.getByText(/select a job from the left/i)).toBeInTheDocument();
  });

  it('renders all stages from a Done JobDetail', async () => {
    renderDetail('job-done');
    await waitFor(() =>
      expect(screen.getByText('scout result text')).toBeInTheDocument(),
    );
    expect(screen.getByText('plan result text')).toBeInTheDocument();
    expect(screen.getByText('build result text')).toBeInTheDocument();
    // Each specialist label appears once on its own row; the
    // stage-name pill ("scout"/"plan"/"build") collides with
    // header pills + list-row text in fuller renders, but here
    // the standalone detail only shows the row pill so the count
    // is well-defined.
    expect(screen.getByText('planner')).toBeInTheDocument();
    expect(screen.getByText('backend-builder')).toBeInTheDocument();
    // "scout" appears twice in this fixture: once as the stage
    // pill, once as the specialist label (specialist_id="scout").
    expect(screen.getAllByText('scout').length).toBeGreaterThanOrEqual(1);
  });

  it('shows neither Cancel nor Rerun on a Done job', async () => {
    renderDetail('job-done');
    await waitFor(() => expect(screen.getByText('a finished goal')).toBeInTheDocument());
    expect(screen.queryByRole('button', { name: /cancel/i })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /rerun/i })).not.toBeInTheDocument();
  });

  it('shows Rerun on a Failed job (and surfaces the error message)', async () => {
    renderDetail('job-failed');
    await waitFor(() => expect(screen.getByText('a failed goal')).toBeInTheDocument());
    expect(screen.getByRole('button', { name: /rerun/i })).toBeInTheDocument();
    expect(screen.getByText('something went wrong')).toBeInTheDocument();
    // Cancel hidden on terminal jobs.
    expect(screen.queryByRole('button', { name: /^cancel$/i })).not.toBeInTheDocument();
  });
});

import { describe, expect, it, vi, beforeEach } from 'vitest';
import { renderHook, waitFor, act } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import type { EventCallback } from '@tauri-apps/api/event';

// Mock the bindings layer so the hook sees a controlled
// `swarmListJobs` and `swarmGetJob`.
vi.mock('../lib/bindings', () => ({
  commands: {
    swarmListJobs: vi.fn(),
    swarmGetJob: vi.fn(),
  },
}));

// Capture the most recent `listen` handler so the test can drive
// it directly — Tauri's runtime isn't initialised in jsdom.
let capturedHandler: EventCallback<unknown> | null = null;
let capturedChannel: string | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: (channel: string, handler: EventCallback<unknown>) => {
    capturedChannel = channel;
    capturedHandler = handler;
    return Promise.resolve(() => {});
  },
}));

import { useSwarmJobs } from './useSwarmJobs';
import { useSwarmJob } from './useSwarmJob';
import type { JobDetail, JobSummary, SwarmJobEvent } from '../lib/bindings';

function wrapper(client: QueryClient) {
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  };
}

const JOBS_OK = {
  status: 'ok' as const,
  data: [
    {
      id: 'job-1',
      workspaceId: 'default',
      goal: 'goal-1',
      createdAtMs: Date.now() - 60_000,
      finishedAtMs: null,
      state: 'build' as const,
      stageCount: 2,
      totalCostUsd: 0.02,
      lastError: null,
    },
  ] satisfies JobSummary[],
};

const JOB_DETAIL_OK = {
  status: 'ok' as const,
  data: {
    id: 'job-1',
    workspaceId: 'default',
    goal: 'goal-1',
    createdAtMs: Date.now() - 60_000,
    finishedAtMs: null,
    state: 'build' as const,
    retryCount: 0,
    stages: [],
    lastError: null,
    totalCostUsd: 0,
    totalDurationMs: 0,
  } satisfies JobDetail,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmListJobs).mockResolvedValue(JOBS_OK);
  vi.mocked(commands.swarmGetJob).mockResolvedValue(JOB_DETAIL_OK);
  capturedHandler = null;
  capturedChannel = null;
});

describe('useSwarmJobs', () => {
  it('fetches the job list and surfaces it to the consumer', async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useSwarmJobs('default'), { wrapper: wrapper(qc) });
    await waitFor(() => expect(result.current.data?.length).toBe(1));
    expect(result.current.data?.[0]?.id).toBe('job-1');
  });

  it('refetches the list when useSwarmJob receives a `finished` event', async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    // Mount both hooks: the list hook seeds `['swarm-jobs']`; the
    // detail hook subscribes and pushes a `finished` event.
    renderHook(() => useSwarmJobs('default'), { wrapper: wrapper(qc) });
    renderHook(() => useSwarmJob('job-1'), { wrapper: wrapper(qc) });

    const { commands } = await import('../lib/bindings');
    await waitFor(() => expect(commands.swarmListJobs).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(capturedHandler).not.toBeNull());
    expect(capturedChannel).toBe('swarm:job:job-1:event');

    const event: SwarmJobEvent = {
      kind: 'finished',
      job_id: 'job-1',
      outcome: {
        jobId: 'job-1',
        finalState: 'done',
        stages: [],
        lastError: null,
        totalCostUsd: 0.0,
        totalDurationMs: 0,
      },
    };
    // Drive the listener — emulates Tauri delivering the event.
    await act(async () => {
      capturedHandler!({ event: 'swarm:job:job-1:event', id: 1, payload: event });
    });
    // The hook calls `qc.invalidateQueries({queryKey:['swarm-jobs']})`
    // on `finished`, which triggers a fresh fetch.
    await waitFor(() => expect(commands.swarmListJobs).toHaveBeenCalledTimes(2));
  });
});

import { describe, expect, it } from 'vitest';
import { applySwarmEventToJobDetail } from './useSwarmJob';
import type { JobDetail, StageResult, SwarmJobEvent } from '../lib/bindings';

// Smoke fixture — every test starts from a Scout-stage running
// job with no stages persisted yet. Each case applies one event
// and asserts the helper's mutations.
function makeJob(): JobDetail {
  return {
    id: 'job-1',
    workspaceId: 'default',
    goal: 'ship the swarm UI',
    createdAtMs: 1000,
    finishedAtMs: null,
    state: 'scout',
    retryCount: 0,
    stages: [],
    lastError: null,
    totalCostUsd: 0,
    totalDurationMs: 0,
  };
}

function makeStage(state: 'scout' | 'plan' | 'build', cost: number, dur: number): StageResult {
  return {
    state,
    specialistId: state,
    assistantText: `${state} output`,
    sessionId: `sess-${state}`,
    totalCostUsd: cost,
    durationMs: dur,
  };
}

describe('applySwarmEventToJobDetail', () => {
  it('returns prev unchanged on `started`', () => {
    const prev = makeJob();
    const event: SwarmJobEvent = {
      kind: 'started',
      job_id: prev.id,
      workspace_id: 'default',
      goal: prev.goal,
      created_at_ms: prev.createdAtMs,
    };
    expect(applySwarmEventToJobDetail(prev, event)).toBe(prev);
  });

  it('advances `state` on `stage_started`', () => {
    const prev = makeJob();
    const event: SwarmJobEvent = {
      kind: 'stage_started',
      job_id: prev.id,
      state: 'plan',
      specialist_id: 'planner',
      prompt_preview: 'plan the work',
    };
    const next = applySwarmEventToJobDetail(prev, event);
    expect(next.state).toBe('plan');
    // Stages and cost untouched.
    expect(next.stages).toEqual([]);
    expect(next.totalCostUsd).toBe(0);
  });

  it('appends a stage and accumulates cost / duration on `stage_completed`', () => {
    const prev = makeJob();
    const stage = makeStage('scout', 0.0125, 1234);
    const event: SwarmJobEvent = {
      kind: 'stage_completed',
      job_id: prev.id,
      stage,
    };
    const next = applySwarmEventToJobDetail(prev, event);
    expect(next.stages).toEqual([stage]);
    expect(next.totalCostUsd).toBeCloseTo(0.0125);
    expect(next.totalDurationMs).toBe(1234);
    // FSM controls the state transitions; helper preserves the
    // current state and waits for the next stage_started.
    expect(next.state).toBe(prev.state);
  });

  it('replaces outcome fields on `finished`', () => {
    const prev = makeJob();
    const finalStages = [
      makeStage('scout', 0.01, 1000),
      makeStage('plan', 0.02, 2000),
      makeStage('build', 0.03, 3000),
    ];
    const event: SwarmJobEvent = {
      kind: 'finished',
      job_id: prev.id,
      outcome: {
        jobId: prev.id,
        finalState: 'done',
        stages: finalStages,
        lastError: null,
        totalCostUsd: 0.06,
        totalDurationMs: 6000,
      },
    };
    const next = applySwarmEventToJobDetail(prev, event);
    expect(next.state).toBe('done');
    expect(next.stages).toBe(finalStages);
    expect(next.totalCostUsd).toBeCloseTo(0.06);
    expect(next.totalDurationMs).toBe(6000);
    expect(next.lastError).toBe(null);
    // `finishedAtMs` lands when the event fires (Date.now()).
    expect(next.finishedAtMs).not.toBe(null);
  });

  it('carries `lastError` from outcome on `finished` for failed jobs', () => {
    const prev = makeJob();
    const event: SwarmJobEvent = {
      kind: 'finished',
      job_id: prev.id,
      outcome: {
        jobId: prev.id,
        finalState: 'failed',
        stages: [makeStage('scout', 0.01, 1000)],
        lastError: 'cancelled by user',
        totalCostUsd: 0.01,
        totalDurationMs: 1000,
      },
    };
    const next = applySwarmEventToJobDetail(prev, event);
    expect(next.state).toBe('failed');
    expect(next.lastError).toBe('cancelled by user');
  });

  it('returns prev unchanged on `cancelled`', () => {
    const prev = makeJob();
    const event: SwarmJobEvent = {
      kind: 'cancelled',
      job_id: prev.id,
      cancelled_during: 'build',
    };
    expect(applySwarmEventToJobDetail(prev, event)).toBe(prev);
  });
});

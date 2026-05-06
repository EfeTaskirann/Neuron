// `useSwarmJob(jobId)` — full job detail + live state via the
// `swarm:job:{id}:event` channel (W3-12c). Mirrors `useRun`'s
// snapshot-then-merge pattern: one `swarm:get_job` round-trip
// seeds the cache, then each event optimistically advances the
// cached `JobDetail` so the UI reflects state transitions
// without waiting on the 5s poll in `useSwarmJobs`.
//
// `applySwarmEventToJobDetail` is exported so the helper can be
// driven directly by tests without spinning up the hook.
import { useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  commands,
  type JobDetail,
  type SwarmJobEvent,
} from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function applySwarmEventToJobDetail(
  prev: JobDetail,
  event: SwarmJobEvent,
): JobDetail {
  switch (event.kind) {
    case 'started':
      // FSM has already minted the job row before this event
      // fires; the cache snapshot is authoritative.
      return prev;
    case 'stage_started':
      return { ...prev, state: event.state };
    case 'stage_completed':
      return {
        ...prev,
        // Keep the current `state` — the next `stage_started`
        // (or `finished`) carries the next FSM transition.
        stages: [...prev.stages, event.stage],
        totalCostUsd: prev.totalCostUsd + event.stage.totalCostUsd,
        totalDurationMs: prev.totalDurationMs + event.stage.durationMs,
      };
    case 'finished':
      return {
        ...prev,
        state: event.outcome.finalState,
        stages: event.outcome.stages,
        lastError: event.outcome.lastError,
        totalCostUsd: event.outcome.totalCostUsd,
        totalDurationMs: event.outcome.totalDurationMs,
        finishedAtMs: prev.finishedAtMs ?? Date.now(),
      };
    case 'cancelled':
      // The subsequent `finished` event carries the terminal
      // state (`Failed` with `last_error = "cancelled by user"`).
      return prev;
    case 'retry_started':
      // W3-12e: the FSM is looping back to PLAN with the
      // rejecting verdict's feedback. Keep the cached `state`
      // and `stages` as-is — the next `stage_started` carries
      // the upcoming Plan transition. We DO bump `retryCount`
      // so the optimistic cache reflects the same shape that
      // `swarm:get_job` would return on the next poll.
      return {
        ...prev,
        retryCount: event.attempt - 1,
        lastVerdict: event.verdict,
      };
    default: {
      const _exhaustive: never = event;
      void _exhaustive;
      return prev;
    }
  }
}

export function useSwarmJob(jobId: string | null) {
  const qc = useQueryClient();
  const query = useQuery<JobDetail>({
    queryKey: ['swarm-job', jobId],
    queryFn: () => unwrap(commands.swarmGetJob(jobId as string)),
    enabled: !!jobId,
  });

  useEffect(() => {
    if (!jobId) return;
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    const channel = `swarm:job:${jobId}:event`;
    listen<SwarmJobEvent>(channel, (event) => {
      const payload = event.payload;
      qc.setQueryData<JobDetail>(['swarm-job', jobId], (prev) => {
        if (!prev) return prev;
        return applySwarmEventToJobDetail(prev, payload);
      });
      if (payload.kind === 'finished') {
        // Refresh the recent-jobs list so the running → terminal
        // flip lands without waiting on the 5s poll.
        qc.invalidateQueries({ queryKey: ['swarm-jobs'] });
      }
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        // Listener registration is best-effort — Tauri rejects
        // when the runtime is not initialised (jsdom tests).
        console.warn('[useSwarmJob] failed to subscribe to', channel, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [jobId, qc]);

  return query;
}

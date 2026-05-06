// `SwarmJobList` — recent jobs panel. Status pill mirrors the
// design-system `pill st-*` tokens: Scout/Plan/Build/Init →
// running blue, Done → green, Failed → red. Clicking a row
// raises `onSelect(jobId)` so the parent route can swap the
// right-hand detail pane.
import type { JobState, JobSummary } from '../lib/bindings';
import { useSwarmJobs } from '../hooks/useSwarmJobs';

interface Props {
  workspaceId: string;
  selectedJobId: string | null;
  onSelect: (jobId: string) => void;
}

export function SwarmJobList({
  workspaceId,
  selectedJobId,
  onSelect,
}: Props): JSX.Element {
  const { data: jobs = [], isLoading, isError, error } = useSwarmJobs(workspaceId);

  if (isLoading) {
    return <div className="swarm-list-empty">Loading jobs…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (jobs.length === 0) {
    return (
      <div className="swarm-list-empty">
        No jobs yet. Type a goal above and click Run.
      </div>
    );
  }
  return (
    <ul className="swarm-list">
      {jobs.map((job: JobSummary) => {
        const running = isRunningState(job.state);
        return (
          <li
            key={job.id}
            className={`swarm-list-row${selectedJobId === job.id ? ' active' : ''}`}
            data-active={selectedJobId === job.id ? 'true' : undefined}
            onClick={() => onSelect(job.id)}
          >
            <div className="swarm-list-row-head">
              <span className={`pill ${pillClass(job.state)}`}>
                {running && <span className="pulse-dot" />}
                {job.state}
              </span>
              <span className="swarm-list-row-time mute">
                {formatRelativeMs(job.createdAtMs)}
              </span>
            </div>
            <div className="swarm-list-row-goal">{job.goal}</div>
          </li>
        );
      })}
    </ul>
  );
}

export function isRunningState(state: JobState): boolean {
  // FSM stages that still allow Cancel. Init is included so a
  // freshly-minted job (pre-`stage_started(scout)`) is also
  // marked cancellable.
  return state === 'init' || state === 'scout' || state === 'plan' || state === 'build';
}

export function pillClass(state: JobState): string {
  if (state === 'done') return 'st-ok';
  if (state === 'failed') return 'st-error';
  return 'st-running';
}

// Inline because the codebase has no shared relative-time util
// (RunsRoute has its own seconds-input variant); this WP gets
// its own ms-input formatter rather than promoting one.
export function formatRelativeMs(ms: number): string {
  const deltaSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (deltaSec < 60) return `${deltaSec}s ago`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m ago`;
  const deltaHr = Math.floor(deltaMin / 60);
  if (deltaHr < 24) return `${deltaHr}h ago`;
  const deltaDay = Math.floor(deltaHr / 24);
  return `${deltaDay}d ago`;
}

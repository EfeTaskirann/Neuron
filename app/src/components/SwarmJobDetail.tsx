// `SwarmJobDetail` — right-pane detail surface. Header summarises
// the job (full goal, state pill, total cost / duration, retry
// counter when retries fired). Stage list is one row per
// `StageResult` with an expand-on-click assistant_text excerpt
// and a per-stage Verdict block when present. Footer shows
// Cancel (non-terminal) and Rerun (Failed only) per WP-W3-14 §4.
//
// Verdict + retry rendering is the W3-14 follow-up
// (post-W3-12d/e): backend ships the data, this surface makes
// it visible.
import { useState } from 'react';
import type {
  StageResult,
  Verdict,
  VerdictIssue,
} from '../lib/bindings';
import { useSwarmJob } from '../hooks/useSwarmJob';
import { useCancelSwarmJob } from '../hooks/useCancelSwarmJob';
import { useRunSwarmJob } from '../hooks/useRunSwarmJob';
import { isRunningState, pillClass, formatRelativeMs } from './SwarmJobList';

// Mirrors `MAX_RETRIES` const in `swarm/coordinator/fsm.rs`. Kept
// inline (not on the IPC) so changing it server-side requires a
// small follow-up here too — preferable to silent drift.
const MAX_RETRIES = 2;

interface Props {
  jobId: string | null;
  workspaceId: string;
}

export function SwarmJobDetail({ jobId, workspaceId }: Props): JSX.Element {
  const { data: job, isLoading, isError, error } = useSwarmJob(jobId);
  const cancelJob = useCancelSwarmJob();
  const runJob = useRunSwarmJob();

  if (!jobId) {
    return (
      <div className="swarm-detail-empty">Select a job from the left.</div>
    );
  }
  if (isLoading) {
    return <div className="swarm-detail-empty">Loading job…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (!job) {
    return <div className="swarm-detail-empty">Job not found.</div>;
  }
  const running = isRunningState(job.state);
  const failed = job.state === 'failed';
  const retried = job.retryCount > 0;
  return (
    <div className="swarm-detail">
      <header className="swarm-detail-head">
        <div className="swarm-detail-head-row">
          <span className={`pill ${pillClass(job.state)}`}>
            {running && <span className="pulse-dot" />}
            {job.state}
          </span>
          {retried && (
            <span
              className="pill swarm-retry-pill"
              title="FSM looped back via Verdict feedback (W3-12e retry loop)"
            >
              ↻ Attempt {job.retryCount + 1}/{MAX_RETRIES + 1}
            </span>
          )}
          <span className="swarm-detail-meta mute">
            {formatRelativeMs(job.createdAtMs)}
          </span>
          <span className="swarm-detail-meta mute">
            ${job.totalCostUsd.toFixed(4)}
          </span>
          <span className="swarm-detail-meta mute">
            {(job.totalDurationMs / 1000).toFixed(2)}s
          </span>
        </div>
        <div className="swarm-detail-goal">{job.goal}</div>
        {job.lastError && (
          <div className="swarm-detail-error">{job.lastError}</div>
        )}
        {job.lastVerdict && job.lastVerdict.approved === false && (
          <VerdictBlock
            verdict={job.lastVerdict}
            heading="Last verdict (rejected)"
          />
        )}
      </header>

      <section className="swarm-stages">
        {job.stages.length === 0 && running && (
          <div className="swarm-stage-pending">Running…</div>
        )}
        {job.stages.map((stage: StageResult, i: number) => (
          <StageRow key={`${stage.state}-${i}`} stage={stage} />
        ))}
        {job.stages.length === 0 && !running && (
          <div className="swarm-stage-pending mute">No stages recorded.</div>
        )}
      </section>

      <footer className="swarm-detail-foot">
        {running && (
          <button
            type="button"
            className="btn"
            disabled={cancelJob.isPending}
            onClick={() => cancelJob.mutate(job.id)}
          >
            {cancelJob.isPending ? 'Cancelling…' : 'Cancel'}
          </button>
        )}
        {failed && (
          <button
            type="button"
            className="btn primary"
            disabled={runJob.isPending}
            onClick={() => runJob.mutate({ workspaceId, goal: job.goal })}
          >
            {runJob.isPending ? 'Starting…' : 'Rerun'}
          </button>
        )}
      </footer>
    </div>
  );
}

function StageRow({ stage }: { stage: StageResult }): JSX.Element {
  const [expanded, setExpanded] = useState(false);
  const TRUNCATE = 600;
  const text = stage.assistantText;
  const truncated = text.length > TRUNCATE && !expanded;
  const display = truncated ? text.slice(0, TRUNCATE) + '…' : text;
  // Stages always render with the "ok" colour after they're
  // pushed onto `stages` — by FSM contract a stage only appears
  // on the success path. The mid-stage running indicator lives
  // on the header pill, not here. Reviewer / IntegrationTester
  // stages additionally surface their Verdict (approved or
  // not — W3-12d data made visible here).
  const isVerdictStage =
    stage.state === 'review' || stage.state === 'test';
  return (
    <div className="swarm-stage" data-state={stage.state}>
      <div className="swarm-stage-head">
        <span className={`pill ${pillClass('done')}`}>{stage.state}</span>
        <span className="swarm-stage-spec mono">{stage.specialistId}</span>
        <span className="swarm-stage-meta mute">
          {(stage.durationMs / 1000).toFixed(2)}s
        </span>
        <span className="swarm-stage-meta mute">
          ${stage.totalCostUsd.toFixed(4)}
        </span>
        {isVerdictStage && stage.verdict && (
          <span
            className={`pill ${
              stage.verdict.approved ? 'st-ok' : 'st-fail'
            }`}
            title={stage.verdict.summary}
          >
            {stage.verdict.approved ? '✓ approved' : '✗ rejected'}
          </span>
        )}
      </div>
      <div
        className={`swarm-stage-body${truncated ? ' truncated' : ''}`}
        onClick={() => text.length > TRUNCATE && setExpanded((v) => !v)}
        title={text.length > TRUNCATE ? 'Click to expand' : undefined}
      >
        {display}
      </div>
      {isVerdictStage && stage.verdict && (
        <VerdictBlock verdict={stage.verdict} />
      )}
    </div>
  );
}

function VerdictBlock({
  verdict,
  heading,
}: {
  verdict: Verdict;
  heading?: string;
}): JSX.Element {
  return (
    <div
      className={`swarm-verdict ${
        verdict.approved ? 'approved' : 'rejected'
      }`}
    >
      {heading && <div className="swarm-verdict-heading">{heading}</div>}
      <div className="swarm-verdict-summary">{verdict.summary}</div>
      {verdict.issues.length > 0 && (
        <ul className="swarm-verdict-issues">
          {verdict.issues.map((issue, i) => (
            <VerdictIssueRow key={i} issue={issue} />
          ))}
        </ul>
      )}
    </div>
  );
}

function VerdictIssueRow({ issue }: { issue: VerdictIssue }): JSX.Element {
  const loc =
    issue.file && issue.line
      ? `${issue.file}:${issue.line}`
      : issue.file ?? null;
  return (
    <li
      className="swarm-verdict-issue"
      data-severity={issue.severity}
    >
      <span
        className={`pill swarm-sev-${issue.severity}`}
      >
        {issue.severity}
      </span>
      {loc && <span className="swarm-issue-loc mono">{loc}</span>}
      <span className="swarm-issue-msg">{issue.msg}</span>
    </li>
  );
}


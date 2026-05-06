// `SwarmGoalForm` — textarea + Run button. Fires `swarm:run_job`
// against the hardcoded `default` workspace per WP-W3-14 §2; the
// backend IPC blocks for the duration of the job, but the form
// doesn't `await` — the per-job event channel drives the UI's
// live state and `onSettled` invalidates the list so the new
// row appears as soon as the FSM exits.
import { useState } from 'react';
import { NIcon } from './icons';
import { useRunSwarmJob } from '../hooks/useRunSwarmJob';

interface Props {
  workspaceId: string;
}

export function SwarmGoalForm({ workspaceId }: Props): JSX.Element {
  const [goal, setGoal] = useState('');
  const runJob = useRunSwarmJob();

  function handleSubmit(e: React.FormEvent<HTMLFormElement>): void {
    e.preventDefault();
    const trimmed = goal.trim();
    if (!trimmed) return;
    runJob.mutate(
      { workspaceId, goal: trimmed },
      {
        onSuccess: () => {
          setGoal('');
        },
      },
    );
  }

  return (
    <form className="swarm-goal-form" onSubmit={handleSubmit}>
      <textarea
        className="swarm-goal-input"
        placeholder="Describe a goal — the swarm will scout, plan, and build."
        value={goal}
        onChange={(e) => setGoal(e.target.value)}
        disabled={runJob.isPending}
        rows={3}
      />
      <div className="swarm-goal-actions">
        <button
          type="submit"
          className="btn primary"
          disabled={runJob.isPending || goal.trim().length === 0}
        >
          <NIcon name="play" size={12} />
          <span>{runJob.isPending ? 'Running…' : 'Run'}</span>
        </button>
      </div>
    </form>
  );
}

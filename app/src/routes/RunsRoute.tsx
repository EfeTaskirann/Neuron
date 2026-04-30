// Ports `Neuron Design/app/routes.jsx::RunsRoute`. Filter chips
// drive a client-side state; the data comes from `useRuns()`.
// Backend-side filter via `commands.runsList(filter)` would also
// work, but the prototype filters in JS and the dataset stays
// small enough that round-tripping per chip click is wasteful.
import { useState, useMemo } from 'react';
import { useRuns } from '../hooks/useRuns';
import type { Run } from '../lib/bindings';

type StatusFilter = 'all' | 'running' | 'success' | 'error';

export function RunsRoute(): JSX.Element {
  const { data: runs = [], isLoading, isError, error } = useRuns();
  const [filter, setFilter] = useState<StatusFilter>('all');
  const filtered = useMemo(
    () => (filter === 'all' ? runs : runs.filter((r: Run) => r.status === filter)),
    [runs, filter],
  );
  // Aggregate stats are derived over the FULL set, not the filtered
  // view, so the chip selection doesn't change the headline numbers.
  const totalCost = runs.reduce((sum, r) => sum + r.cost, 0);
  const totalTokens = runs.reduce((sum, r) => sum + r.tokens, 0);

  if (isLoading) {
    return <div className="route route-runs route-loading">Loading runs…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  return (
    <div className="route route-runs">
      <div className="runs-toolbar">
        <div className="chip-row">
          {(['all', 'running', 'success', 'error'] as const).map((f) => (
            <button
              key={f}
              className={`chip${filter === f ? ' active' : ''}`}
              onClick={() => setFilter(f)}
            >
              {f === 'running' && <span className="pulse-dot" />}
              {f}
            </button>
          ))}
        </div>
        <div className="runs-stats">
          <span>
            <b>{runs.length}</b> runs
          </span>
          <span>
            <b>${totalCost.toFixed(4)}</b> total
          </span>
          <span>
            <b>{(totalTokens / 1000).toFixed(1)}k</b> tokens
          </span>
        </div>
      </div>
      <table className="runs-table">
        <thead>
          <tr>
            <th>id</th>
            <th>workflow</th>
            <th>started</th>
            <th>duration</th>
            <th>tokens</th>
            <th>cost</th>
            <th>status</th>
          </tr>
        </thead>
        <tbody>
          {filtered.map((r: Run) => (
            <tr key={r.id}>
              <td className="mono">{r.id}</td>
              <td>{r.workflow}</td>
              <td className="mute">{formatStarted(r.startedAt)}</td>
              <td>{r.dur != null ? `${(r.dur / 1000).toFixed(2)}s` : '—'}</td>
              <td>{r.tokens.toLocaleString()}</td>
              <td>${r.cost.toFixed(4)}</td>
              <td>
                <span
                  className={`pill st-${
                    r.status === 'success'
                      ? 'ok'
                      : r.status === 'running'
                      ? 'running'
                      : 'error'
                  }`}
                >
                  {r.status === 'running' && <span className="pulse-dot" />}
                  {r.status}
                </span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// Charter Constraint #1 carve-out: backend ships `started_at` as
// UNIX seconds; the hook layer derives the "2 min ago" string.
// Implementation is local to this route for now — promote to a
// shared util once a second consumer needs it.
function formatStarted(seconds: number): string {
  const deltaSec = Math.max(0, Math.floor(Date.now() / 1000) - seconds);
  if (deltaSec < 60) return `${deltaSec}s ago`;
  const deltaMin = Math.floor(deltaSec / 60);
  if (deltaMin < 60) return `${deltaMin}m ago`;
  const deltaHr = Math.floor(deltaMin / 60);
  if (deltaHr < 24) return `${deltaHr}h ago`;
  const deltaDay = Math.floor(deltaHr / 24);
  return `${deltaDay}d ago`;
}

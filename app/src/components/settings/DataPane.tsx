// Data settings pane. Surfaces local run-history stats from `runs:list`
// and offers a client-side JSON export. Everything lives in a local
// SQLite database on this machine — nothing is synced.
import { useRuns } from '../../hooks/useRuns';

export function DataPane(): JSX.Element {
  const runs = useRuns();
  const list = runs.data ?? [];
  const totalCost = list.reduce((sum, r) => sum + r.cost, 0);
  const exportRuns = (): void => {
    const blob = new Blob([JSON.stringify(list, null, 2)], {
      type: 'application/json',
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `neuron-runs-${list.length}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Data
      </h2>
      <p className="text-muted">
        Your runs, agents, and settings live in a local SQLite database on
        this machine — nothing is synced.
      </p>
      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Run history</div>
            <div className="set-row-sub">
              {runs.isLoading
                ? 'Loading…'
                : `${list.length} runs · $${totalCost.toFixed(4)} total`}
            </div>
          </div>
          <button
            type="button"
            className="btn ghost sm"
            onClick={exportRuns}
            disabled={runs.isLoading || list.length === 0}
          >
            Export JSON
          </button>
        </div>
      </div>
    </>
  );
}

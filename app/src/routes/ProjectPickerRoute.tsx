import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { Brandmark } from '../components/icons';
import { useActiveProject } from '../hooks/useActiveProject';

// Full-screen landing shown by `<App>` when no active project is
// stored. Picking a folder writes to the `useActiveProject` store,
// which the App component reads via `useSyncExternalStore` and
// immediately drops the gate — the user lands on the standard
// sidebar + main layout one render tick later.
//
// Rendered outside the sidebar/topbar shell on purpose: until a
// project is picked, no other tab is meaningful, so showing the
// chrome is just visual noise.

export function ProjectPickerRoute(): JSX.Element {
  const { setProject } = useActiveProject();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pick = async () => {
    setPending(true);
    setError(null);
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === 'string' && selected.length > 0) {
        setProject(selected);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setPending(false);
    }
  };

  return (
    <div className="project-picker-route">
      <div className="project-picker-card">
        <div className="project-picker-brand">
          <Brandmark size={48} />
          <h1>Neuron</h1>
        </div>
        <h2 className="project-picker-title">Pick a project</h2>
        <p className="project-picker-subtitle">
          Choose the folder Neuron will operate on. Terminal Swarm and the
          plain Terminal will run with this folder as their working
          directory; the other tabs are workspace-global but the header
          will show which project is active.
        </p>
        <button
          type="button"
          className="btn primary lg project-picker-cta"
          onClick={pick}
          disabled={pending}
          autoFocus
        >
          {pending ? 'Picking…' : 'Browse folder…'}
        </button>
        {error && <div className="project-picker-error">{error}</div>}
        <p className="project-picker-hint">
          You can change projects later from the header chip.
        </p>
      </div>
    </div>
  );
}

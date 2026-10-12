import { useState, type FormEvent } from 'react';
import { NIcon } from '../icons';
import { useActiveProject } from '../../hooks/useActiveProject';
import {
  useTerminalPurgeClosed,
  useTerminalSpawn,
} from '../../hooks/mutations';
import type { Pane } from '../../lib/bindings';
import { TERMINAL_STATUSES } from './agentMeta';

// Inline spawn dialog. Button collapses into a small form; submit
// calls terminal:spawn with the typed cwd. cmd/cols/rows fall
// back to the platform default per WP-W2-06's ergonomics.
//
// Default cwd is the App-level active project folder (if set);
// the user can still edit the field for one-off spawns elsewhere.
// Pre-2026-05-13 this defaulted to literal `.` (process CWD = the
// Neuron .exe install dir), which was almost never what the user
// wanted.
export function NewPaneButton(): JSX.Element {
  const spawn = useTerminalSpawn();
  const { project } = useActiveProject();
  const defaultCwd = project?.path ?? '.';
  const [open, setOpen] = useState(false);
  const [cwd, setCwd] = useState(defaultCwd);

  if (!open) {
    return (
      <button className="btn primary" onClick={() => setOpen(true)}>
        <NIcon name="plus" size={14} />
        <span>New pane</span>
      </button>
    );
  }
  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (!cwd.trim()) return;
    spawn.mutate(
      {
        cwd: cwd.trim(),
        cmd: null,
        cols: null,
        rows: null,
        agentKind: null,
        role: null,
        workspace: null,
        extraEnv: null,
      },
      {
        onSuccess: () => {
          setOpen(false);
          setCwd(defaultCwd);
        },
      },
    );
  };
  return (
    <form className="new-pane-form" onSubmit={handleSubmit}>
      <input
        autoFocus
        value={cwd}
        onChange={(e) => setCwd(e.target.value)}
        placeholder="cwd (e.g. ~/work)"
        aria-label="Working directory"
      />
      <button type="submit" className="btn primary sm" disabled={spawn.isPending}>
        {spawn.isPending ? 'Spawning…' : 'Spawn'}
      </button>
      <button
        type="button"
        className="btn ghost sm"
        onClick={() => setOpen(false)}
      >
        Cancel
      </button>
    </form>
  );
}

// "Clean closed" — bulk-removes closed/error/success panes from the
// DB so the tab strip stops accumulating after each swarm launch.
// Disabled when nothing is purgeable so users don't fire a no-op.
export function PurgeClosedButton({ panes }: { panes: Pane[] }): JSX.Element {
  const purge = useTerminalPurgeClosed();
  const purgeable = panes.filter((p) => TERMINAL_STATUSES.has(p.status)).length;
  const disabled = purgeable === 0 || purge.isPending;
  return (
    <button
      type="button"
      className="btn ghost sm"
      disabled={disabled}
      onClick={() => purge.mutate()}
      title={
        purgeable === 0
          ? 'No closed panes'
          : `Remove ${purgeable} closed/errored pane${purgeable === 1 ? '' : 's'}`
      }
    >
      <NIcon name="trash" size={12} />
      <span>
        {purge.isPending
          ? 'Cleaning…'
          : `Clean closed${purgeable > 0 ? ` (${purgeable})` : ''}`}
      </span>
    </button>
  );
}

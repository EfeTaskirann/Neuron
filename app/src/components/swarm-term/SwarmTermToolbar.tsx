import { SessionTimer } from './SessionTimer';

interface SwarmTermToolbarProps {
  /** Globally-selected project path (topbar chip); used as the readout
   *  fallback + title when no session is running. */
  projectDir: string | null;
  /** `session.projectDir` while a session is active, else `null`. */
  sessionProjectDir: string | null;
  sessionActive: boolean;
  canLaunch: boolean;
  startPending: boolean;
  stopPending: boolean;
  /** Decoded launch time (ms) — mounts <SessionTimer> when non-null. */
  sessionStartMs: number | null;
  /** Whether the Routing Log drawer is open. */
  logOpen: boolean;
  onLaunch: () => void;
  onRestart: () => void;
  onStop: () => void;
  onToggleLog: () => void;
}

export function SwarmTermToolbar({
  projectDir,
  sessionProjectDir,
  sessionActive,
  canLaunch,
  startPending,
  stopPending,
  sessionStartMs,
  logOpen,
  onLaunch,
  onRestart,
  onStop,
  onToggleLog,
}: SwarmTermToolbarProps): JSX.Element {
  return (
    <div className="swarm-term-toolbar">
      {/*
        The inline ProjectPicker is gone — the project is established
        globally at app launch via <ProjectPickerRoute /> and shown in
        the topbar chip. The toolbar now displays the path read-only
        and the lifecycle buttons (Launch / Restart / Stop).
      */}
      <div className="swarm-term-project-readout" title={projectDir ?? ''}>
        <span className="swarm-term-project-label">Project:</span>
        <span className="swarm-term-project-path">
          {sessionActive ? sessionProjectDir : projectDir ?? '(none)'}
        </span>
      </div>
      {!sessionActive ? (
        <button
          type="button"
          className="btn primary sm"
          disabled={!canLaunch}
          onClick={onLaunch}
          title="Launch the 9-agent swarm"
        >
          {startPending ? 'Launching…' : 'Launch swarm'}
        </button>
      ) : (
        <>
          <button
            type="button"
            className="btn ghost sm"
            onClick={onRestart}
            disabled={startPending || stopPending}
            title="Stop and re-launch with the same project"
          >
            {startPending || stopPending ? 'Restarting…' : 'Restart'}
          </button>
          <button
            type="button"
            className="btn ghost sm"
            onClick={onStop}
            disabled={stopPending}
          >
            {stopPending ? 'Stopping…' : 'Stop swarm'}
          </button>
        </>
      )}
      {sessionStartMs != null && <SessionTimer startMs={sessionStartMs} />}
      <button
        type="button"
        className="btn ghost sm swarm-term-log-toggle"
        aria-pressed={logOpen}
        onClick={onToggleLog}
        title={logOpen ? 'Hide the Routing Log' : 'Show every message between the agents'}
      >
        Routing Log
      </button>
    </div>
  );
}

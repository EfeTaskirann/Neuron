import type { TerminalSwarmPaneState } from '../../lib/bindings';
import { READINESS_LABEL } from './constants';
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
  /** Whether the Changes panel is open, and how many files it lists. */
  changesOpen: boolean;
  changesCount: number | null;
  /** The user paused the running swarm (and a pause / resume is on its way). */
  paused: boolean;
  pausePending: boolean;
  /** Every pane's readiness while a session runs (empty otherwise). */
  paneStates: TerminalSwarmPaneState[];
  onLaunch: () => void;
  onRestart: () => void;
  onStop: () => void;
  onToggleLog: () => void;
  onToggleChanges: () => void;
  onTogglePause: () => void;
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
  changesOpen,
  changesCount,
  paused,
  pausePending,
  paneStates,
  onLaunch,
  onRestart,
  onStop,
  onToggleLog,
  onToggleChanges,
  onTogglePause,
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
          <button
            type="button"
            className="btn ghost sm swarm-term-log-toggle"
            aria-pressed={paused}
            onClick={onTogglePause}
            disabled={pausePending}
            title={
              paused
                ? 'Mesaj teslimini ve hatırlatmaları sürdür'
                : 'Mesaj teslimini ve hatırlatmaları duraklat (çalışan tur biter)'
            }
          >
            {paused ? 'Sürdür' : 'Duraklat'}
          </button>
        </>
      )}
      {sessionStartMs != null && <SessionTimer startMs={sessionStartMs} />}
      {sessionActive && paneStates.length > 0 && <ReadyBadge paneStates={paneStates} />}
      <button
        type="button"
        className="btn ghost sm swarm-term-log-toggle"
        aria-pressed={logOpen}
        onClick={onToggleLog}
        title={logOpen ? 'Hide the Routing Log' : 'Show every message between the agents'}
      >
        Routing Log
      </button>
      <button
        type="button"
        className="btn ghost sm swarm-term-log-toggle"
        aria-pressed={changesOpen}
        onClick={onToggleChanges}
        title={
          changesOpen
            ? 'Değişiklikler panelini kapat'
            : 'Ajanların checkpoint’ten beri değiştirdiği dosyalar'
        }
      >
        {changesCount ? `Değişiklikler (${changesCount})` : 'Değişiklikler'}
      </button>
    </div>
  );
}

const PROBLEM_STATES = ['blocked', 'not_ready', 'exited'];

/** "n/9 ready", with who is not and why in the tooltip / description. */
function ReadyBadge({ paneStates }: { paneStates: TerminalSwarmPaneState[] }): JSX.Element {
  const ready = paneStates.filter((p) => p.readiness === 'ready').length;
  const waiting = paneStates.filter((p) => p.readiness !== 'ready');
  const all = waiting.length === 0;
  const problem = waiting.some((p) => PROBLEM_STATES.includes(p.readiness));
  const details = waiting
    .map((p) => `@${p.agentId}: ${READINESS_LABEL[p.readiness]}${p.detail ? ` — ${p.detail}` : ''}`)
    .join('\n');
  return (
    <span
      className={`swarm-term-ready${all ? ' swarm-term-ready--all' : ''}${
        problem ? ' swarm-term-ready--problem' : ''
      }`}
      role="status"
      title={details || 'Every agent answered the kickoff'}
      aria-label={`${ready} of ${paneStates.length} agents ready${details ? `. ${details}` : ''}`}
    >
      {ready}/{paneStates.length} ready
    </span>
  );
}

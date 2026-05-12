import { useState, type FormEvent } from 'react';
import { ProjectPicker } from '../components/ProjectPicker';
import { RoutingOverlay } from '../components/RoutingOverlay';
import { SwarmPane } from '../components/SwarmPane';
import { useTerminalWrite } from '../hooks/mutations';
import {
  useStartSwarmTermSession,
  useStopSwarmTermSession,
  useSwarmTermPersonas,
  useSwarmTermSessionStatus,
} from '../hooks/useTerminalSwarmSession';

const SLOT_ORDER = [
  'orchestrator',
  'coordinator',
  'scout',
  'planner',
  'backend-builder',
  'frontend-builder',
  'backend-reviewer',
  'frontend-reviewer',
  'integration-tester',
];

export function TerminalSwarmRoute(): JSX.Element {
  const [projectDir, setProjectDir] = useState<string | null>(null);
  const [launchError, setLaunchError] = useState<string | null>(null);
  const [chatInput, setChatInput] = useState('');
  const { data: personas = [] } = useSwarmTermPersonas();
  const { data: session } = useSwarmTermSessionStatus();
  const startMut = useStartSwarmTermSession();
  const stopMut = useStopSwarmTermSession();
  const writeMut = useTerminalWrite();

  const sessionActive = session != null;
  const canLaunch =
    projectDir != null && !sessionActive && !startMut.isPending;

  const launch = async () => {
    if (!projectDir) return;
    setLaunchError(null);
    try {
      await startMut.mutateAsync(projectDir);
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const stop = async () => {
    try {
      await stopMut.mutateAsync();
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const restart = async () => {
    if (!sessionActive) return;
    const dir = session.projectDir;
    setLaunchError(null);
    try {
      await stopMut.mutateAsync();
      await startMut.mutateAsync(dir);
    } catch (err) {
      setLaunchError(err instanceof Error ? err.message : String(err));
    }
  };

  const personasById = new Map(personas.map((p) => [p.id, p]));
  const panesByAgent = new Map(
    session?.panes.map((p) => [p.agentId, p.paneId]) ?? [],
  );
  const orchestratorPaneId = panesByAgent.get('orchestrator') ?? null;

  const sendToOrchestrator = (e: FormEvent) => {
    e.preventDefault();
    const msg = chatInput.trim();
    if (!msg || !orchestratorPaneId) return;
    writeMut.mutate({
      paneId: orchestratorPaneId,
      data: `${msg}\r`,
    });
    setChatInput('');
  };

  return (
    <div className="route route-swarm-term">
      <div className="swarm-term-toolbar">
        <ProjectPicker
          value={sessionActive ? session.projectDir : projectDir}
          onChange={setProjectDir}
          disabled={sessionActive}
        />
        {!sessionActive ? (
          <button
            type="button"
            className="btn primary sm"
            disabled={!canLaunch}
            onClick={launch}
            title={
              projectDir == null
                ? 'Pick a project first'
                : 'Launch the 9-agent swarm'
            }
          >
            {startMut.isPending ? 'Launching…' : 'Launch swarm'}
          </button>
        ) : (
          <>
            <button
              type="button"
              className="btn ghost sm"
              onClick={restart}
              disabled={startMut.isPending || stopMut.isPending}
              title="Stop and re-launch with the same project"
            >
              {startMut.isPending || stopMut.isPending
                ? 'Restarting…'
                : 'Restart'}
            </button>
            <button
              type="button"
              className="btn ghost sm"
              onClick={stop}
              disabled={stopMut.isPending}
            >
              {stopMut.isPending ? 'Stopping…' : 'Stop swarm'}
            </button>
          </>
        )}
      </div>

      {launchError && (
        <div className="swarm-term-error">{launchError}</div>
      )}

      {sessionActive && orchestratorPaneId && (
        <form className="swarm-term-chat" onSubmit={sendToOrchestrator}>
          <input
            type="text"
            placeholder="Talk to @orchestrator — type your task and hit Enter"
            value={chatInput}
            onChange={(e) => setChatInput(e.target.value)}
          />
          <button
            type="submit"
            className="btn primary sm"
            disabled={!chatInput.trim()}
          >
            Send
          </button>
        </form>
      )}

      <RoutingOverlay />

      <div className="swarm-term-grid">
        {SLOT_ORDER.map((agentId) => {
          const persona = personasById.get(agentId);
          const paneId = panesByAgent.get(agentId);
          return (
            <div
              key={agentId}
              className={`swarm-term-pane${paneId ? '' : ' swarm-term-pane-empty'}`}
            >
              <div className="swarm-term-pane-head">
                <span className="swarm-term-pane-id">@{agentId}</span>
                <span className="swarm-term-pane-role">
                  {persona?.role ?? '—'}
                </span>
              </div>
              <div className="swarm-term-pane-body">
                {paneId ? (
                  <SwarmPane paneId={paneId} agentId={agentId} />
                ) : (
                  <em>idle — pick a project and launch</em>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

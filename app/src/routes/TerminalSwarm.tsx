import { useMemo, useRef, useState } from 'react';
import { HierarchyDiagram } from '../components/SwarmHierarchy';
import { ClaudeUpdateCorner } from '../components/swarm-term/ClaudeUpdateCorner';
import { OrchestratorChatBar } from '../components/swarm-term/OrchestratorChatBar';
import { SwarmTermGrid } from '../components/swarm-term/SwarmTermGrid';
import { SwarmTermToolbar } from '../components/swarm-term/SwarmTermToolbar';
import { parseSwarmTermStartMs } from '../components/swarm-term/constants';
import { useActiveProject } from '../hooks/useActiveProject';
import { useTerminalWrite } from '../hooks/mutations';
import {
  useRunClaudeUpdate,
  useStartSwarmTermSession,
  useStopSwarmTermSession,
  useSwarmTermPersonas,
  useSwarmTermSessionStatus,
} from '../hooks/useTerminalSwarmSession';
import { useClaudeUpdateProgress } from '../hooks/useClaudeUpdateProgress';
import {
  useActiveEdge,
  useAgentLifecycle,
  useRoutingEvents,
} from '../hooks/useRoutingEvents';
import { AppErrorClient } from '../lib/unwrap';
import { RoutingLogRoute } from './RoutingLogRoute';

/** Gap between the pasted chat message and its submitting CR. */
const SUBMIT_DELAY_MS = 200;

/**
 * The line shown for a failed launch / stop / send: the error kind's copy
 * plus the backend's detail. `AppErrorClient.message` is only the kind's
 * generic copy, so without the detail a refused launch would never say
 * why (e.g. which folder the project guard rejected).
 */
export function describeError(err: unknown): string {
  if (err instanceof AppErrorClient && err.detail && err.detail !== err.message) {
    return `${err.message} ${err.detail}`;
  }
  return err instanceof Error ? err.message : String(err);
}

/**
 * Make chat text safe inside a bracketed paste (mirrors the backend's
 * `sanitize_for_paste`): CR/CRLF → LF and every control character except
 * LF/TAB — C0 incl. ESC, DEL, C1 — removed. Dropping ESC itself is what
 * stops `\x1b\x1b[201~` from re-forming the paste-end sequence after a
 * one-pass replace and leaking the rest as keystrokes.
 */
export function sanitizeForPaste(text: string): string {
  return Array.from(text.replace(/\r\n?/g, '\n'))
    .filter((ch) => {
      const code = ch.codePointAt(0) ?? 0;
      const control = code < 0x20 || (code >= 0x7f && code <= 0x9f);
      return ch === '\n' || ch === '\t' || !control;
    })
    .join('');
}

export function TerminalSwarmRoute(): JSX.Element {
  // Project comes from the App-level gate — the route is never
  // rendered with `project == null` because <ProjectPickerRoute />
  // takes over the whole window in that case. We still read defensively
  // from the hook so a future deep-link / forced-route can't crash
  // here.
  const { project } = useActiveProject();
  const projectDir = project?.path ?? null;
  const [launchError, setLaunchError] = useState<string | null>(null);
  // The Routing Log keeps its history in a module store, so closing the
  // drawer loses nothing.
  const [logOpen, setLogOpen] = useState(false);
  const { events: routeEvents } = useRoutingEvents(500);
  const activeEdge = useActiveEdge(routeEvents, 3_000);
  const lifecycle = useAgentLifecycle(routeEvents);

  const { data: personas = [] } = useSwarmTermPersonas();
  const {
    data: session,
    isLoading: sessionLoading,
    isError: sessionProbeFailed,
    error: sessionProbeError,
  } = useSwarmTermSessionStatus();
  const startMut = useStartSwarmTermSession();
  const stopMut = useStopSwarmTermSession();
  const updateMut = useRunClaudeUpdate();
  const updateProgress = useClaudeUpdateProgress(updateMut.isPending);
  const writeMut = useTerminalWrite();

  const sessionActive = session != null;
  // `!sessionLoading`: while the status probe is in flight we don't
  // yet know whether a session exists — launching then would only
  // bounce off the backend's Conflict guard.
  const canLaunch =
    projectDir != null && !sessionActive && !sessionLoading && !startMut.isPending;

  const launch = async () => {
    if (!projectDir) return;
    setLaunchError(null);
    try {
      await startMut.mutateAsync(projectDir);
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const stop = async () => {
    try {
      await stopMut.mutateAsync();
    } catch (err) {
      setLaunchError(describeError(err));
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
      setLaunchError(describeError(err));
    }
  };

  const personasById = useMemo(
    () => new Map(personas.map((p) => [p.id, p])),
    [personas],
  );
  const panesByAgent = useMemo(
    () => new Map(session?.panes.map((p) => [p.agentId, p.paneId]) ?? []),
    [session],
  );
  const orchestratorPaneId = panesByAgent.get('orchestrator') ?? null;

  // Uptime ticking lives in <SessionTimer> (inside the toolbar) so its
  // 1 s interval re-renders ~20px of text instead of this whole route —
  // which would otherwise reconcile the 3×3 grid and all 9 SwarmPanes
  // every second.
  const sessionStartMs = session ? parseSwarmTermStartMs(session.sessionId) : null;

  // Bracketed paste, then the submitting CR as a separate, delayed write
  // (mirrors the backend's `paste_then_submit`). A CR in the same write
  // as a long / multi-line message is absorbed by claude's TUI as part
  // of the paste, leaving the task sitting unsent in the input box.
  // Sends are chained so a quick second message can't interleave its
  // paste between the first one's paste and CR (which would merge both
  // texts into one prompt and submit an empty line).
  const sendChainRef = useRef<Promise<void>>(Promise.resolve());
  const sendToOrchestrator = (message: string) => {
    if (!orchestratorPaneId) return;
    const paneId = orchestratorPaneId;
    const safe = sanitizeForPaste(message);
    sendChainRef.current = sendChainRef.current.then(async () => {
      try {
        await writeMut.mutateAsync({ paneId, data: `\x1b[200~${safe}\x1b[201~` });
        await new Promise((resolve) => setTimeout(resolve, SUBMIT_DELAY_MS));
        await writeMut.mutateAsync({ paneId, data: '\r' });
      } catch (err) {
        setLaunchError(describeError(err));
      }
    });
  };

  return (
    <div className="route route-swarm-term">
      <div className="swarm-term-main">
        <SwarmTermToolbar
          projectDir={projectDir}
          sessionProjectDir={session?.projectDir ?? null}
          sessionActive={sessionActive}
          canLaunch={canLaunch}
          startPending={startMut.isPending}
          stopPending={stopMut.isPending}
          sessionStartMs={sessionStartMs}
          logOpen={logOpen}
          onLaunch={launch}
          onRestart={restart}
          onStop={stop}
          onToggleLog={() => setLogOpen((open) => !open)}
        />

        {launchError && (
          <div className="swarm-term-error" role="alert">
            {launchError}
          </div>
        )}
        {sessionProbeFailed && (
          <div className="swarm-term-error" role="alert">
            Session status unavailable: {describeError(sessionProbeError)}
          </div>
        )}

        <HierarchyDiagram
          lifecycle={lifecycle}
          activeSource={activeEdge?.source ?? null}
          activeTarget={activeEdge?.target ?? null}
          personasById={personasById}
          panesByAgent={panesByAgent}
        />

        {sessionActive && orchestratorPaneId && (
          <OrchestratorChatBar
            onSend={sendToOrchestrator}
            permissionMode={session.permissionMode}
          />
        )}

        <SwarmTermGrid
          personasById={personasById}
          panesByAgent={panesByAgent}
          lifecycle={lifecycle}
          activeEdge={activeEdge}
        />
      </div>

      {logOpen && (
        <aside className="swarm-term-log-drawer" aria-label="Routing Log">
          <RoutingLogRoute />
        </aside>
      )}

      <ClaudeUpdateCorner
        sessionActive={sessionActive}
        isPending={updateMut.isPending}
        progress={updateProgress}
        result={updateMut.data}
        error={updateMut.error}
        onUpdate={() => updateMut.mutate()}
      />
    </div>
  );
}

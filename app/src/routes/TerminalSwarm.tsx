import { useMemo, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { ask } from '@tauri-apps/plugin-dialog';
import { HierarchyDiagram } from '../components/SwarmHierarchy';
import { ChangesDrawer } from '../components/swarm-term/ChangesDrawer';
import { ClaudeUpdateCorner } from '../components/swarm-term/ClaudeUpdateCorner';
import { OrchestratorChatBar } from '../components/swarm-term/OrchestratorChatBar';
import { SwarmTermGrid } from '../components/swarm-term/SwarmTermGrid';
import { SwarmTermToolbar } from '../components/swarm-term/SwarmTermToolbar';
import { parseSwarmTermStartMs } from '../components/swarm-term/constants';
import { useActiveProject } from '../hooks/useActiveProject';
import { useTerminalWrite } from '../hooks/mutations';
import {
  useExtendSwarmTermBudget,
  useRestartSwarmTermAgent,
  useResumeSwarmTermDeliveries,
  useRunClaudeUpdate,
  useSetSwarmTermPaused,
  useStartSwarmTermSession,
  useStopSwarmTermSession,
  useSwarmTermAlerts,
  useSwarmTermPersonas,
  useSwarmTermSessionStatus,
  useTrustSwarmTermProject,
} from '../hooks/useTerminalSwarmSession';
import { useClaudeUpdateProgress } from '../hooks/useClaudeUpdateProgress';
import { fetchSwarmTermChanges, useSwarmTermChanges } from '../hooks/useSwarmTermChanges';
import { useRoutingEvents } from '../hooks/useRoutingEvents';
import { useActiveEdge, useAgentLifecycle } from '../hooks/useAgentLifecycle';
import { formatResetTime } from '../lib/swarmNotify';
import { AppErrorClient, describeError } from '../lib/unwrap';
import type { TerminalSwarmPaneState } from '../lib/bindings';
import { RoutingLogRoute } from './RoutingLogRoute';

/** Gap between the pasted chat message and its submitting CR. */
const SUBMIT_DELAY_MS = 200;

/** Why @orchestrator cannot take a task yet, or `null` when it can. */
function chatWaitReason(state: TerminalSwarmPaneState | undefined): string | null {
  switch (state?.readiness) {
    case 'starting':
      return 'Waiting for @orchestrator to start…';
    case 'kickoff_sent':
      return 'Waiting for @orchestrator to answer its kickoff…';
    case 'blocked':
      return `@orchestrator needs you: ${state.detail ?? 'answer the screen in its pane'}`;
    case 'not_ready':
      return '@orchestrator did not answer its kickoff — check its pane';
    case 'exited':
      return "@orchestrator's claude exited — restart it with ↻ on its pane";
    default:
      return null;
  }
}

/**
 * Asked once per folder, before the first launch there (the backend
 * answers `consent_required`). A yes is recorded; the session then
 * answers Claude Code's own "do you trust this folder?" for the panes.
 */
function askToTrustFolder(folder: string): Promise<boolean> {
  return ask(
    `Swarm şu klasörde çalışacak:\n${folder}\n\n` +
      'Ajanlar buradaki dosyaları okur, builder’lar değiştirir. Testleri ve build’i ' +
      'çalıştırmak projenin kendi kodunu senin yetkilerinle çalıştırır. Projenin commit’li ' +
      '`.claude/` ayarları da (hook’lar, izinler) bu yetkilerle çalışabilir. Yalnız ' +
      'güvendiğin bir projede devam et.\n\n' +
      'Her başlangıçta klasörün o anki hali bir git checkpoint’i olarak saklanır: ' +
      'ajanların değişikliklerini Değişiklikler panelinde görüp geri alabilirsin ' +
      '(git deposu olmayan bir klasörde bu yapılamaz).\n\n' +
      'Ajanlar senin Claude girişini paylaştığı için, onaylarsan Claude Code’un bu ' +
      'klasör için sorduğu “güveniyor musun?” sorusu senin adına “evet” yanıtlanır; bu ' +
      'güven kalıcıdır ve bu klasörde kendi `claude` oturumların da bir daha sormaz.',
    {
      title: 'Bu klasöre güveniyor musun?',
      kind: 'warning',
      okLabel: 'Güveniyorum, başlat',
      cancelLabel: 'Vazgeç',
    },
  );
}

/** Stop asks first: every agent's claude session ends with it. */
function confirmStop(agents: number): Promise<boolean> {
  return ask(
    `Swarm durdurulsun mu?\n\n${agents} ajanın claude oturumu kapanır; yarım kalan işleri ` +
      'kaybolur. Değişiklikler panelindeki checkpoint kalır.',
    { title: 'Swarm’ı durdur', kind: 'warning', okLabel: 'Durdur', cancelLabel: 'Vazgeç' },
  );
}

/** Restart asks first: the agents start over, and so does the checkpoint. */
function confirmRestart(agents: number): Promise<boolean> {
  return ask(
    `Swarm yeniden başlatılsın mı?\n\n${agents} ajan kapanıp sıfırdan açılır; konuşmaları ve ` +
      'yarım kalan işleri kaybolur. Yeni oturum yeni bir checkpoint alır: şimdiye kadarki ' +
      'değişiklikler artık Değişiklikler panelinden geri alınamaz.',
    { title: 'Swarm’ı yeniden başlat', kind: 'warning', okLabel: 'Yeniden başlat', cancelLabel: 'Vazgeç' },
  );
}

/** A launch over changes the user has not kept or reverted asks first. */
function confirmNewCheckpoint(files: number): Promise<boolean> {
  return ask(
    `Önceki oturumun ${files} dosyalık değişikliği henüz tutulmadı ya da geri alınmadı.\n\n` +
      'Yeni başlangıç yeni bir checkpoint alır: bu değişiklikler artık Değişiklikler ' +
      'panelinden geri alınamaz. Yine de başlatılsın mı?',
    { title: 'Swarm’ı başlat', kind: 'warning', okLabel: 'Başlat', cancelLabel: 'Vazgeç' },
  );
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
  // One drawer at a time: the Routing Log (its history lives in a module
  // store, so closing it loses nothing) or the Changes panel.
  const [drawer, setDrawer] = useState<'log' | 'changes' | null>(null);
  const toggleDrawer = (which: 'log' | 'changes') =>
    setDrawer((open) => (open === which ? null : which));
  const { events: routeEvents } = useRoutingEvents(500);
  const activeEdge = useActiveEdge(routeEvents, 3_000);

  const { data: personas = [] } = useSwarmTermPersonas();
  const {
    data: session,
    isLoading: sessionLoading,
    isError: sessionProbeFailed,
    error: sessionProbeError,
  } = useSwarmTermSessionStatus();
  const lifecycle = useAgentLifecycle(routeEvents, session?.sessionId ?? null);
  useSwarmTermAlerts();
  const paneStates = useMemo(() => session?.paneStates ?? [], [session]);
  const paneStatesByAgent = useMemo(
    () => new Map(paneStates.map((p) => [p.agentId, p])),
    [paneStates],
  );
  const chatDisabledReason = chatWaitReason(paneStatesByAgent.get('orchestrator'));
  const startMut = useStartSwarmTermSession();
  const trustMut = useTrustSwarmTermProject();
  const stopMut = useStopSwarmTermSession();
  const restartAgentMut = useRestartSwarmTermAgent();
  const resumeMut = useResumeSwarmTermDeliveries();
  const pauseMut = useSetSwarmTermPaused();
  const budgetMut = useExtendSwarmTermBudget();
  const updateMut = useRunClaudeUpdate();
  const updateProgress = useClaudeUpdateProgress(updateMut.isPending);
  // Send failures land in this route's error banner.
  const writeMut = useTerminalWrite({ errorsShownInline: true });
  const qc = useQueryClient();

  const sessionActive = session != null;
  // The running session's folder, else the active project (its last
  // session's checkpoint): polled while the agents work.
  const changesProject = session?.projectDir ?? projectDir;
  const changesQuery = useSwarmTermChanges(changesProject, {
    enabled: sessionActive || drawer === 'changes',
    live: sessionActive,
  });
  // `!sessionLoading`: while the status probe is in flight we don't
  // yet know whether a session exists — launching then would only
  // bounce off the backend's Conflict guard.
  const canLaunch =
    projectDir != null &&
    !sessionActive &&
    !sessionLoading &&
    !startMut.isPending &&
    !trustMut.isPending;

  // Start, asking first if the user has not trusted the folder yet. A
  // "no" is not an error: nothing starts and nothing is reported.
  const startWithConsent = async (dir: string) => {
    try {
      await startMut.mutateAsync(dir);
    } catch (err) {
      if (!(err instanceof AppErrorClient) || err.kind !== 'consent_required') throw err;
      if (!(await askToTrustFolder(err.detail || dir))) return;
      await trustMut.mutateAsync(dir);
      await startMut.mutateAsync(dir);
    }
  };

  const launch = async () => {
    if (!projectDir) return;
    setLaunchError(null);
    // A start takes a new checkpoint: changes since the last one would no
    // longer be revertible from the panel. A failed read does not block.
    const pending = await fetchSwarmTermChanges(qc, projectDir)
      .then((c) => c.files.length)
      .catch(() => 0);
    if (pending > 0 && !(await confirmNewCheckpoint(pending))) return;
    try {
      await startWithConsent(projectDir);
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const stop = async () => {
    if (!(await confirmStop(session?.panes.length ?? 0))) return;
    try {
      await stopMut.mutateAsync();
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const restart = async () => {
    if (!sessionActive) return;
    if (!(await confirmRestart(session.panes.length))) return;
    const dir = session.projectDir;
    setLaunchError(null);
    try {
      await stopMut.mutateAsync();
      await startWithConsent(dir);
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const restartAgent = async (agentId: string) => {
    setLaunchError(null);
    try {
      await restartAgentMut.mutateAsync(agentId);
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const togglePause = async () => {
    if (!sessionActive) return;
    setLaunchError(null);
    try {
      await pauseMut.mutateAsync(!session.paused);
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const extendBudget = async () => {
    setLaunchError(null);
    try {
      await budgetMut.mutateAsync();
    } catch (err) {
      setLaunchError(describeError(err));
    }
  };

  const resumeDeliveries = async () => {
    setLaunchError(null);
    try {
      await resumeMut.mutateAsync();
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
          logOpen={drawer === 'log'}
          changesOpen={drawer === 'changes'}
          changesCount={changesQuery.data?.files.length ?? null}
          paused={session?.paused ?? false}
          pausePending={pauseMut.isPending}
          paneStates={paneStates}
          onLaunch={launch}
          onRestart={restart}
          onStop={stop}
          onToggleLog={() => toggleDrawer('log')}
          onToggleChanges={() => toggleDrawer('changes')}
          onTogglePause={togglePause}
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
        {session?.preflightNote && (
          <div className="swarm-term-note" role="status">
            {session.preflightNote}
          </div>
        )}
        {session?.changesNote && (
          <div className="swarm-term-note" role="status">
            {session.changesNote}
          </div>
        )}
        {session?.paused && (
          <div className="swarm-term-note" role="status">
            Swarm duraklatıldı: mesajlar outbox’larda sırayla bekliyor, takılma hatırlatmaları
            durdu. Çalışan bir ajan elindeki turu bitirir.
          </div>
        )}
        {session?.budgetReached && (
          <div className="swarm-term-limit" role="alert">
            <p>
              <strong>Mesaj bütçesi doldu</strong> ({session.messagesDelivered}/
              {session.messageBudget}). Ajanlar arası mesaj teslimi ve takılma hatırlatmaları
              durdu.
            </p>
            <button
              type="button"
              className="btn"
              onClick={extendBudget}
              disabled={budgetMut.isPending}
            >
              Bütçeyi artır, sürdür
            </button>
          </div>
        )}
        {session?.usageLimited && (
          <div className="swarm-term-limit" role="alert">
            <p>
              <strong>Claude kullanım limitine ulaşıldı</strong>
              {session.limitResetsAt != null &&
                ` (sıfırlanma: ${formatResetTime(session.limitResetsAt)})`}
              . Mesaj teslimi ve takılma hatırlatmaları duraklatıldı. Limit sıfırlanınca
              claude genelde kendiliğinden sürer; bir ajan yeniden yanıt verince swarm da
              devam eder.
            </p>
            <button
              type="button"
              className="btn"
              onClick={resumeDeliveries}
              disabled={resumeMut.isPending}
            >
              Şimdi sürdür
            </button>
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
            disabledReason={chatDisabledReason}
          />
        )}

        <SwarmTermGrid
          personasById={personasById}
          panesByAgent={panesByAgent}
          lifecycle={lifecycle}
          activeEdge={activeEdge}
          paneStates={paneStatesByAgent}
          onRestartAgent={restartAgent}
          liteTeam={session?.liteTeam ?? false}
          restartingAgent={restartAgentMut.isPending ? (restartAgentMut.variables ?? null) : null}
        />
      </div>

      {drawer === 'log' && (
        <aside className="swarm-term-log-drawer" aria-label="Routing Log">
          <RoutingLogRoute />
        </aside>
      )}
      {drawer === 'changes' && changesProject && (
        <aside className="swarm-term-log-drawer" aria-label="Değişiklikler">
          <ChangesDrawer
            projectDir={changesProject}
            changes={changesQuery.data}
            updatedAt={changesQuery.dataUpdatedAt}
            fetching={changesQuery.isFetching}
            error={changesQuery.error}
            onRefresh={() => void changesQuery.refetch()}
          />
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

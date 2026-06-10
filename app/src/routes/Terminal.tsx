// Ports `Neuron Design/app/terminal.jsx::TerminalRoute`. Backend
// data sources: usePanes() (snapshot of every pane) +
// usePaneLines(paneId) (per-pane scrollback + live line events).
//
// Tab-strip layout: panes accumulate over time (swarm launches each
// produce 9 panes), and trying to render every xterm at once melts
// the renderer + spams live-line subscriptions for dead PTYs. The
// route shows every pane in a horizontally-scrollable tab strip
// and mounts an xterm only for the *active* tab. Closed/error panes
// stay visible (their scrollback hydrates from the persisted
// `pane_lines` table) until the user hits "Clean closed".
//
// This file is the orchestration layer only — the pane components
// live under `components/terminal/` (T2-01 split).
import { useMemo, useState } from 'react';
import { usePanes } from '../hooks/usePanes';
import { MailboxPanel } from '../components/terminal/MailboxPanel';
import { PaneTabStrip } from '../components/terminal/PaneTabStrip';
import { PaneView } from '../components/terminal/PaneView';
import { TermStatusBar } from '../components/terminal/TermStatusBar';
import {
  NewPaneButton,
  PurgeClosedButton,
} from '../components/terminal/TerminalToolbar';

export function TerminalRoute(): JSX.Element {
  const { data: panes = [], isLoading, isError, error } = usePanes();
  const [activeId, setActiveId] = useState<string | null>(null);

  // Resolve the active pane at render time so a purged-out
  // `activeId` falls back to the first live pane without an effect
  // round-trip. The state still owns the user's last click; this
  // memo just narrows it to whatever exists right now.
  const resolvedActiveId = useMemo<string | null>(() => {
    if (panes.length === 0) return null;
    if (activeId != null && panes.some((p) => p.id === activeId)) return activeId;
    return panes[0]!.id;
  }, [panes, activeId]);

  if (isLoading) {
    return <div className="term-route route-loading">Loading panes…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  if (panes.length === 0) {
    return (
      <div className="term-route term-route-empty">
        <p className="text-muted">No panes yet. Spawn one to get started.</p>
        <NewPaneButton />
      </div>
    );
  }
  const activePane =
    panes.find((p) => p.id === resolvedActiveId) ?? panes[0]!;
  return (
    <div className="term-route">
      <MailboxPanel />
      <div className="term-toolbar">
        <NewPaneButton />
        <PurgeClosedButton panes={panes} />
      </div>
      <div className="pane-main">
        <PaneTabStrip
          panes={panes}
          activeId={activePane.id}
          onSelect={setActiveId}
        />
        <div className="pane-active">
          <PaneView
            key={activePane.id}
            pane={activePane}
            active
            onActivate={() => setActiveId(activePane.id)}
          />
        </div>
      </div>
      <TermStatusBar panes={panes} />
    </div>
  );
}

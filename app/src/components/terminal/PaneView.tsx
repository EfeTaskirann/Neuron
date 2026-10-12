import { useRef, type MutableRefObject } from 'react';
import { NIcon } from '../icons';
import { useTerminalKill, useTerminalWrite } from '../../hooks/mutations';
import { useXtermPane } from '../../hooks/useXtermPane';
import type { Pane } from '../../lib/bindings';
import { AgentIcon } from './AgentIcon';
import {
  metaFor,
  STATUS_LABEL,
  TERMINAL_STATUSES,
  type AgentInfo,
} from './agentMeta';

interface PaneViewProps {
  pane: Pane;
  active: boolean;
  onActivate: () => void;
}

export function PaneView({ pane, active, onActivate }: PaneViewProps): JSX.Element {
  const agent = metaFor(pane.agent);
  // PaneBody owns the xterm instance; it registers its `clear()` here so
  // the header's Clear button (a sibling component) can invoke it without
  // lifting the whole instance up.
  const clearRef = useRef<(() => void) | null>(null);
  return (
    <div
      className={`pane status-${pane.status}${active ? ' active' : ''}`}
      onClick={onActivate}
    >
      <div className="pane-stripe" />
      <PaneHeader
        pane={pane}
        agent={agent}
        onClear={() => clearRef.current?.()}
      />
      {pane.approval && (
        <ApprovalBanner paneId={pane.id} approval={pane.approval} />
      )}
      <PaneBody pane={pane} clearRef={clearRef} />
    </div>
  );
}

function PaneHeader({
  pane,
  agent,
  onClear,
}: {
  pane: Pane;
  agent: AgentInfo;
  onClear: () => void;
}): JSX.Element {
  const kill = useTerminalKill();
  return (
    <div className="pane-head">
      <div className="pane-head-l">
        <AgentIcon kind={agent.icon} accent={agent.accent} />
        <span className="pane-name">{agent.name}</span>
        <span className={`pane-dot status-${pane.status}`} />
        <span className="pane-status">{STATUS_LABEL[pane.status] ?? pane.status}</span>
        {pane.role && <span className="pane-role">· {pane.role}</span>}
      </div>
      <div className="pane-cwd" title={pane.cwd}>
        {pane.cwd}
      </div>
      <div className="pane-head-r">
        <button
          className="icon-btn sm"
          title="Clear scrollback"
          aria-label="Clear scrollback"
          onClick={(e) => {
            e.stopPropagation();
            onClear();
          }}
        >
          <NIcon name="trash" size={12} />
        </button>
        {/* Restart / Pop-out stubs removed: no backend command exists
            for either yet — fake controls on a real pane erode trust.
            Re-add alongside the real IPCs. */}
        <button
          className="icon-btn sm"
          title="Close pane"
          aria-label="Close pane"
          disabled={kill.isPending}
          onClick={(e) => {
            e.stopPropagation();
            kill.mutate(pane.id);
          }}
        >
          <NIcon name="close" size={12} />
        </button>
      </div>
    </div>
  );
}

// Accept/Reject answer the agent's pending y/n prompt by writing the
// keystrokes straight to the PTY — there is no dedicated approval IPC;
// the agent process itself owns the prompt, we just type for the user.
function ApprovalBanner({
  paneId,
  approval,
}: {
  paneId: string;
  approval: NonNullable<Pane['approval']>;
}): JSX.Element {
  const write = useTerminalWrite();
  const answer = (keys: string) => {
    if (write.isPending) return;
    write.mutate({ paneId, data: keys });
  };
  return (
    <div className="approval-banner">
      <span className="ab-tag">tool</span>
      <code className="ab-tool">{approval.tool}</code>
      <span className="ab-arrow">→</span>
      <code className="ab-target">{approval.target}</code>
      <span className="ab-diff">
        <span className="ab-add">+{approval.added}</span>
        <span className="ab-rem">−{approval.removed}</span>
      </span>
      <div className="ab-spacer" />
      <button
        className="btn ghost sm"
        disabled={write.isPending}
        onClick={() => answer('n\r')}
      >
        Reject
      </button>
      <button
        className="btn primary sm"
        disabled={write.isPending}
        onClick={() => answer('y\r')}
      >
        Accept
      </button>
    </div>
  );
}

// xterm-backed pane body — a thin wrapper over the shared
// `useXtermPane` hook (mount/teardown, write, resize, snapshot
// hydration, live stream; see the hook for the ordering contract).
// Dead PTYs (closed/error/success) mount with `live: false`: no
// event will ever fire and the PTY behind the resize IPC is gone.
//
// Backend currently strips ANSI before emitting (see
// terminal.rs::LineEventPayload — `text` is plain). xterm still
// gives us a real cursor, scroll, font, and input handling; ANSI
// rendering follows when the backend event payload changes.
function PaneBody({
  pane,
  clearRef,
}: {
  pane: Pane;
  clearRef?: MutableRefObject<(() => void) | null>;
}): JSX.Element {
  const containerRef = useXtermPane(pane.id, {
    fontSize: 12,
    live: !TERMINAL_STATUSES.has(pane.status),
    clearRef,
  });
  return <div className="pane-body pane-body-xterm" ref={containerRef} />;
}

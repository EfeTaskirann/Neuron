// `AgentPane` — single pane in the W4-04 3×3 swarm grid. One
// instance per agent; subscribes to its per-(ws, agent) event
// channel via `useAgentEvents` and reads its status pill from the
// `useAgentStatuses` poll.
//
// Structure:
//   ┌───────────────────────────────────────┐
//   │ [name]  [status pill]  [N turns]      │  ← header
//   ├───────────────────────────────────────┤
//   │ event 1                               │
//   │ event 2 (live transcript scrollback)  │  ← body
//   │ event 3 …                             │
//   ├───────────────────────────────────────┤
//   │ $0.0042 · last activity 12s           │  ← footer
//   └───────────────────────────────────────┘
//
// Body renders a structured event stream rather than a plain ANSI
// terminal — stream-json is already parsed, so xterm round-tripping
// is wasted work. Each event kind has a dedicated bubble class so
// CSS can colour-code (assistant_text neutral, tool_use accented,
// help_request flagged).
import { useMemo, useRef, useEffect } from 'react';
import type { AgentStatus, AgentStatusRow, SwarmAgentEvent } from '../lib/bindings';
import { useAgentEvents } from '../hooks/useAgentEvents';

interface Props {
  workspaceId: string;
  agentId: string;
  displayName: string;
  status: AgentStatusRow | null;
}

export function AgentPane({
  workspaceId,
  agentId,
  displayName,
  status,
}: Props): JSX.Element {
  const events = useAgentEvents(workspaceId, agentId);
  const bodyRef = useRef<HTMLDivElement>(null);

  // Auto-scroll the transcript to the latest event. Mirrors
  // OrchestratorChatPanel's history ref pattern.
  useEffect(() => {
    if (bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [events.length]);

  const pillStatus = status?.status ?? 'not_spawned';
  const turnsTaken = status?.turnsTaken ?? 0;
  const lastActivityMs = status?.lastActivityMs ?? null;

  // Last cost from the most recent Result event (events have it;
  // status row doesn't carry it). Walk events backwards.
  const totalCostUsd = useMemo(() => {
    for (let i = events.length - 1; i >= 0; i -= 1) {
      const e = events[i]!;
      if (e.kind === 'result') {
        return e.total_cost_usd;
      }
    }
    return 0;
  }, [events]);

  return (
    <div
      className={`agent-pane status-${pillStatus}`}
      data-agent-id={agentId}
    >
      <div className="agent-pane-head">
        <span className="agent-pane-name">{displayName}</span>
        <AgentStatusPill status={pillStatus} />
        {turnsTaken > 0 && (
          <span
            className="agent-pane-turns mono"
            title="Turns this session has taken"
          >
            {turnsTaken}t
          </span>
        )}
      </div>
      <div className="agent-pane-body" ref={bodyRef}>
        {events.length === 0 && pillStatus === 'not_spawned' && (
          <div className="agent-pane-empty mute">Idle — not yet spawned.</div>
        )}
        {events.length === 0 && pillStatus !== 'not_spawned' && (
          <div className="agent-pane-empty mute">Waiting for first turn…</div>
        )}
        {events.map((ev, i) => (
          <AgentEventBubble key={i} event={ev} />
        ))}
      </div>
      <div className="agent-pane-foot">
        <span className="agent-pane-cost mono">
          ${totalCostUsd.toFixed(4)}
        </span>
        <span className="agent-pane-foot-sep mute">·</span>
        <span className="agent-pane-activity mute">
          {formatLastActivity(lastActivityMs)}
        </span>
      </div>
    </div>
  );
}

function AgentStatusPill({ status }: { status: AgentStatus }): JSX.Element {
  const label: Record<AgentStatus, string> = {
    not_spawned: 'idle',
    spawning: 'spawning',
    idle: 'idle',
    running: 'running',
    waiting_on_coordinator: 'waiting',
    crashed: 'crashed',
  };
  const running = status === 'running' || status === 'spawning';
  return (
    <span className={`pill agent-pill agent-pill-${status}`}>
      {running && <span className="pulse-dot" aria-hidden="true" />}
      {label[status]}
    </span>
  );
}

function AgentEventBubble({
  event,
}: {
  event: SwarmAgentEvent;
}): JSX.Element {
  switch (event.kind) {
    case 'spawned':
      return (
        <div className="agent-event ev-spawned mute">
          <span className="ev-tag">spawned</span>
          <span className="ev-text mono">{event.profile_id}</span>
        </div>
      );
    case 'turn_started':
      return (
        <div className="agent-event ev-turn-started mute">
          <span className="ev-tag">turn {event.turn_index + 1}</span>
        </div>
      );
    case 'assistant_text':
      // Streaming deltas land here. The body is plain text — no
      // markdown rendering yet (a follow-up could add it).
      return (
        <div className="agent-event ev-assistant-text">
          {event.delta}
        </div>
      );
    case 'tool_use':
      return (
        <div className="agent-event ev-tool-use">
          <span className="ev-tag">tool</span>
          <code className="ev-tool-name mono">{event.name}</code>
          {event.input_summary.length > 0 && (
            <span className="ev-tool-input mono mute">
              {event.input_summary}
            </span>
          )}
        </div>
      );
    case 'result':
      return (
        <div className="agent-event ev-result">
          <span className="ev-tag">result</span>
          <span className="ev-meta mute mono">
            {event.turn_count}t · ${event.total_cost_usd.toFixed(4)}
          </span>
          <div className="ev-result-text">{event.assistant_text}</div>
        </div>
      );
    case 'help_request':
      // Reserved for W4-05; render as a flagged bubble even
      // though no event source emits it yet.
      return (
        <div className="agent-event ev-help-request">
          <span className="ev-tag">help</span>
          <span className="ev-help-reason">{event.reason}</span>
          <div className="ev-help-question">{event.question}</div>
        </div>
      );
    case 'idle':
      return (
        <div className="agent-event ev-idle mute">
          <span className="ev-tag">idle</span>
        </div>
      );
    case 'crashed':
      return (
        <div className="agent-event ev-crashed">
          <span className="ev-tag">crashed</span>
          <span className="ev-error">{event.error}</span>
        </div>
      );
    default: {
      const _exhaustive: never = event;
      void _exhaustive;
      return <div className="agent-event mute">unknown event</div>;
    }
  }
}

function formatLastActivity(ms: number | null): string {
  if (ms === null) return '—';
  const delta = Date.now() - ms;
  if (delta < 1000) return 'just now';
  if (delta < 60_000) return `${Math.floor(delta / 1000)}s ago`;
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m ago`;
  return `${Math.floor(delta / 3_600_000)}h ago`;
}

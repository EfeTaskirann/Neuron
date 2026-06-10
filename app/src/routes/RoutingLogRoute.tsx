import { useMemo, useState } from 'react';
import {
  useRoutingEvents,
  ALL_OUTCOMES,
  type RouteOutcome,
} from '../hooks/useRoutingEvents';

// Standalone Routing Log route. Subscribes to `swarm-term:route` via
// `useRoutingEvents` and renders the full-screen event stream with an
// outcome filter and click-to-copy bodies — replaces the older in-tab
// overlay.

const OUTCOME_LABEL: Record<RouteOutcome, string> = {
  ok: 'routed',
  malformed: 'malformed',
  denied: 'denied',
  unknown_target: 'no target',
  target_not_ready: 'not ready',
  target_locked: 'locked',
  target_write_timeout: 'timeout',
  lifecycle_fanout: 'auto-fanout',
};

function formatTs(ts: number): string {
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, '0');
  const mm = String(d.getMinutes()).padStart(2, '0');
  const ss = String(d.getSeconds()).padStart(2, '0');
  const mmm = String(d.getMilliseconds()).padStart(3, '0');
  return `${hh}:${mm}:${ss}.${mmm}`;
}

export function RoutingLogRoute(): JSX.Element {
  const { events, clear } = useRoutingEvents(500);
  const [enabled, setEnabled] = useState<Set<RouteOutcome>>(
    () => new Set(ALL_OUTCOMES),
  );

  const filtered = useMemo(
    () => events.filter((e) => enabled.has(e.outcome)),
    [events, enabled],
  );

  const toggle = (o: RouteOutcome) => {
    setEnabled((prev) => {
      const next = new Set(prev);
      if (next.has(o)) {
        next.delete(o);
      } else {
        next.add(o);
      }
      return next;
    });
  };

  const copyBody = (body: string) => {
    if (typeof navigator === 'undefined' || !navigator.clipboard) return;
    navigator.clipboard.writeText(body).catch(() => {
      /* clipboard refused — ignore */
    });
  };

  return (
    <div className="route route-routing-log">
      <div className="routing-log-header">
        <div className="routing-log-title">
          <span>Routing Log</span>
          <span className="routing-log-counter">
            {filtered.length} / {events.length}
          </span>
        </div>
        <div className="routing-log-actions">
          <button
            type="button"
            className="btn ghost sm"
            onClick={clear}
            disabled={events.length === 0}
          >
            Clear
          </button>
        </div>
      </div>

      <div className="chip-row">
        {ALL_OUTCOMES.map((o) => {
          const active = enabled.has(o);
          return (
            <button
              key={o}
              type="button"
              className={`chip chip-outcome-${o}${active ? ' active' : ''}`}
              onClick={() => toggle(o)}
              title={`Toggle ${o}`}
            >
              {OUTCOME_LABEL[o]}
            </button>
          );
        })}
      </div>

      <div className="routing-log-body">
        {filtered.length === 0 ? (
          <div className="routing-log-empty">
            {events.length === 0
              ? 'No routing events yet. Launch a swarm session and have agents talk to each other.'
              : 'No events match the current filter. Toggle outcomes above to see more.'}
          </div>
        ) : (
          <table className="routing-log-table">
            <thead>
              <tr>
                <th>Time</th>
                <th>Source → Target</th>
                <th>Outcome</th>
                <th>Body</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map((e) => (
                <tr
                  key={e.id}
                  className={`routing-log-row routing-log-row-${e.outcome}`}
                >
                  <td className="routing-log-time">{formatTs(e.ts)}</td>
                  <td className="routing-log-edge">
                    <span className="routing-log-src">@{e.source}</span>
                    <span className="routing-log-arrow">→</span>
                    <span className="routing-log-dst">@{e.target}</span>
                  </td>
                  <td>
                    <span
                      className={`routing-log-tag routing-log-tag-${e.outcome}`}
                      title={e.reason}
                    >
                      {OUTCOME_LABEL[e.outcome]}
                    </span>
                  </td>
                  <td className="routing-log-body-cell">
                    <button
                      type="button"
                      className="routing-log-body-text"
                      title={e.body || '(no body)'}
                      aria-label="Copy message body"
                      onClick={() => copyBody(e.body)}
                    >
                      {e.body
                        ? e.body.length > 200
                          ? `${e.body.slice(0, 200)}…`
                          : e.body
                        : '—'}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}

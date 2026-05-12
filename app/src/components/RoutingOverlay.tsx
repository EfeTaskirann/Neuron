import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

interface RouteEvent {
  source: string;
  target: string;
  body: string;
  outcome: 'ok' | 'denied' | 'unknown_target' | 'near_miss';
  ts: number;
}

interface RawPayload {
  source: string;
  target: string;
  body: string;
  outcome: string;
}

const MAX_ROWS = 50;

export function RoutingOverlay(): JSX.Element {
  const [events, setEvents] = useState<RouteEvent[]>([]);
  const [collapsed, setCollapsed] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<RawPayload>('swarm-term:route', (event) => {
      const p = event.payload;
      const next: RouteEvent = {
        source: p.source,
        target: p.target,
        body: p.body,
        outcome:
          p.outcome === 'ok' ||
          p.outcome === 'denied' ||
          p.outcome === 'unknown_target' ||
          p.outcome === 'near_miss'
            ? p.outcome
            : 'unknown_target',
        ts: Date.now(),
      };
      setEvents((prev) => {
        const out = [next, ...prev];
        if (out.length > MAX_ROWS) out.length = MAX_ROWS;
        return out;
      });
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch((err) => {
        console.warn('[RoutingOverlay] subscribe failed', err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return (
    <div className={`swarm-term-overlay${collapsed ? ' collapsed' : ''}`}>
      <div
        className="swarm-term-overlay-head"
        onClick={() => setCollapsed((c) => !c)}
        role="button"
        title={collapsed ? 'Expand routing log' : 'Collapse routing log'}
      >
        <span className="swarm-term-overlay-title">
          Routing log ({events.length})
        </span>
        <span className="swarm-term-overlay-chev">
          {collapsed ? '▾' : '▴'}
        </span>
      </div>
      {!collapsed && (
        <div className="swarm-term-overlay-body">
          {events.length === 0 ? (
            <div className="swarm-term-overlay-empty">
              No routes yet — the first marker line in any pane will appear here.
            </div>
          ) : (
            <ul className="swarm-term-overlay-list">
              {events.map((e, i) => (
                <li
                  key={`${e.ts}-${i}`}
                  className={`swarm-term-overlay-row swarm-term-overlay-row-${e.outcome}`}
                >
                  <span className="swarm-term-overlay-src">@{e.source}</span>
                  <span className="swarm-term-overlay-arrow">→</span>
                  <span className="swarm-term-overlay-dst">@{e.target}</span>
                  <span className="swarm-term-overlay-body-text" title={e.body}>
                    {e.body.length > 90
                      ? `${e.body.slice(0, 90)}…`
                      : e.body}
                  </span>
                  <span className={`swarm-term-overlay-tag tag-${e.outcome}`}>
                    {e.outcome === 'ok'
                      ? 'routed'
                      : e.outcome === 'near_miss'
                      ? 'format!'
                      : e.outcome}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

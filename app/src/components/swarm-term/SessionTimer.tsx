import { useEffect, useState } from 'react';
import { formatMmSs } from './constants';

// Self-contained uptime clock. Owns its own `now` state + 1 s interval
// so the per-second tick only re-renders this span — not the parent
// route's 3×3 pane grid. Mounts only while a session is active (parent
// gates on `sessionStartMs != null`), so the interval is torn down on
// stop without a guard here.
export function SessionTimer({ startMs }: { startMs: number }): JSX.Element {
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNowMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, []);
  const label = formatMmSs(nowMs - startMs);
  return (
    <span
      className="swarm-term-session-timer"
      title="Session uptime (mm:ss)"
      aria-label={`Session uptime ${label}`}
    >
      {label}
    </span>
  );
}

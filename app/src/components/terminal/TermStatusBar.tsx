import { useMemo } from 'react';
import { NIcon } from '../icons';
import type { Pane } from '../../lib/bindings';

interface TermStatusBarProps {
  panes: Pane[];
}

export function TermStatusBar({ panes }: TermStatusBarProps): JSX.Element {
  const counts = useMemo(() => {
    const c = { running: 0, idle: 0, error: 0, awaiting: 0, success: 0, closed: 0 };
    for (const p of panes) {
      if (p.status === 'running') c.running++;
      else if (p.status === 'idle') c.idle++;
      else if (p.status === 'error') c.error++;
      else if (p.status === 'awaiting_approval') c.awaiting++;
      else if (p.status === 'success') c.success++;
      else if (p.status === 'closed') c.closed++;
    }
    return c;
  }, [panes]);

  return (
    <div className="term-statusbar">
      <div className="tsb-l">
        <div className="tsb-ws">
          <NIcon name="layers" size={12} /> personal · {panes.length} panes
        </div>
      </div>
      <div className="tsb-c">
        {counts.running > 0 && (
          <span className="tsb-pill st-running">
            <span className="pulse-dot" />
            {counts.running} running
          </span>
        )}
        {counts.awaiting > 0 && (
          <span className="tsb-pill st-awaiting">{counts.awaiting} awaiting</span>
        )}
        {counts.success > 0 && (
          <span className="tsb-pill st-success">{counts.success} done</span>
        )}
        {counts.idle > 0 && <span className="tsb-pill st-idle">{counts.idle} idle</span>}
        {counts.error > 0 && (
          <span className="tsb-pill st-error">{counts.error} error</span>
        )}
        {counts.closed > 0 && (
          <span className="tsb-pill st-idle">{counts.closed} closed</span>
        )}
      </div>
      <div className="tsb-r" />
    </div>
  );
}

import { useState } from 'react';
import { useTauriEvent } from './useTauriEvent';

// Live progress feed for the "Update Claude" button. Backend
// (`src-tauri/src/commands/swarm_term.rs`) streams every stdout/stderr
// line from the spawned updater as a `swarm-term:update:log` event so
// the UI can replace the "Updating…" silent stall with a ticking
// last-line indicator. Ring-buffered to N=10 because we only need
// recent context — the full tail lives in the mutation result.

const UPDATE_LOG_EVENT = 'swarm-term:update:log';
const MAX_LINES = 10;

interface UpdateLogPayload {
  stream: 'stdout' | 'stderr';
  line: string;
}

export interface ClaudeUpdateProgress {
  lines: string[];
  lastLine: string | null;
  clear: () => void;
}

export function useClaudeUpdateProgress(enabled: boolean): ClaudeUpdateProgress {
  const [lines, setLines] = useState<string[]>([]);
  const [prevEnabled, setPrevEnabled] = useState(enabled);

  // Fresh-run reset: on a false→true transition, drop stale lines so the
  // operator only sees the current run's output. Done during render (not
  // in an effect) per React's "storing information from previous renders".
  if (enabled !== prevEnabled) {
    setPrevEnabled(enabled);
    if (enabled) setLines([]);
  }

  // A `null` channel while disabled suspends the subscription; it
  // resubscribes on the false→true transition.
  useTauriEvent<UpdateLogPayload>(
    enabled ? UPDATE_LOG_EVENT : null,
    (payload) => {
      const line = payload?.line;
      if (typeof line !== 'string' || line.length === 0) return;
      setLines((prev) => {
        const next =
          prev.length >= MAX_LINES
            ? prev.slice(-(MAX_LINES - 1))
            : prev.slice();
        next.push(line);
        return next;
      });
    },
  );

  return {
    lines,
    lastLine: lines.length > 0 ? lines[lines.length - 1]! : null,
    clear: () => setLines([]),
  };
}

import { useEffect, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

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

  // Fresh-run reset: on a false→true transition, drop stale lines so
  // the operator only sees the current run's output. Done during
  // render (not in an effect) per
  // https://react.dev/reference/react/useState#storing-information-from-previous-renders
  if (enabled !== prevEnabled) {
    setPrevEnabled(enabled);
    if (enabled) setLines([]);
  }

  useEffect(() => {
    if (!enabled) return;
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<UpdateLogPayload>(UPDATE_LOG_EVENT, (event) => {
      const line = event.payload?.line;
      if (typeof line !== 'string' || line.length === 0) return;
      setLines((prev) => {
        const next = prev.length >= MAX_LINES ? prev.slice(-(MAX_LINES - 1)) : prev.slice();
        next.push(line);
        return next;
      });
    })
      .then((fn) => {
        // StrictMode double-mount safety: if the effect was cancelled
        // before listen() resolved, fire the unsubscribe immediately
        // instead of leaking a dangling listener.
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => {
        console.warn('[useClaudeUpdateProgress] subscribe failed', err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [enabled]);

  return {
    lines,
    lastLine: lines.length > 0 ? lines[lines.length - 1]! : null,
    clear: () => setLines([]),
  };
}

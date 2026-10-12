// `usePaneLines(paneId)` — scrollback snapshot via `terminal:lines`
// + live `panes:{id}:line` subscription per ADR-0006. Each event
// payload is a `PaneLine`; we append it to the cached array,
// dropping anything earlier than the snapshot's tail so a slow
// listener doesn't double-insert lines.
//
// Lines have no UI-side reordering — the backend is a single
// writer per pane (the PTY reader task), `seq` is monotonic per
// pane, so append-only is safe.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type PaneLine } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { useTauriEvent } from './useTauriEvent';

export function usePaneLines(
  paneId: string | null | undefined,
  opts?: { live?: boolean },
) {
  const live = opts?.live ?? true;
  const qc = useQueryClient();
  const query = useQuery<PaneLine[]>({
    queryKey: ['panes', paneId, 'lines'],
    queryFn: () => unwrap(commands.terminalLines(paneId as string, null)),
    enabled: !!paneId,
  });

  // Live subscription is opt-in. The xterm-backed consumers (SwarmPane,
  // Terminal's PaneBody) render new lines by writing to xterm directly
  // from their own `panes:{id}:line` listener, so they only need the
  // one-shot snapshot from the query above. Keeping the live listener
  // on for them would (a) double the IPC subscription per pane — 9 panes
  // → 18 listeners — and (b) grow this React-Query array unbounded:
  // xterm caps its own scrollback at 5000 lines, this array does not.
  // A future plain-React log viewer that renders from `query.data` can
  // opt back in with `{ live: true }`.
  useTauriEvent<PaneLine>(
    paneId && live ? `panes:${paneId}:line` : null,
    (incoming) => {
      qc.setQueryData<PaneLine[]>(['panes', paneId, 'lines'], (prev = []) => {
        // Backend is the single writer per pane; `seq` is monotonic
        // ascending. A tail check rejects duplicates AND late arrivals
        // in O(1), avoiding the O(n²) cost of a full `.some()` scan
        // when scrollback grows into the thousands.
        const last = prev[prev.length - 1];
        if (last && incoming.seq <= last.seq) return prev;
        return [...prev, incoming];
      });
    },
  );

  return query;
}

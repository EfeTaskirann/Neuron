// `usePaneLines(paneId)` — scrollback snapshot via `terminal:lines`
// + live `panes:{id}:line` subscription per ADR-0006. Each event
// payload is a `PaneLine`; we append it to the cached array,
// dropping anything earlier than the snapshot's tail so a slow
// listener doesn't double-insert lines.
//
// Lines have no UI-side reordering — the backend is a single
// writer per pane (the PTY reader task), `seq` is monotonic per
// pane, so append-only is safe.
import { useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { commands, type PaneLine } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function usePaneLines(paneId: string | null | undefined) {
  const qc = useQueryClient();
  const query = useQuery<PaneLine[]>({
    queryKey: ['panes', paneId, 'lines'],
    queryFn: () => unwrap(commands.terminalLines(paneId as string, null)),
    enabled: !!paneId,
  });

  useEffect(() => {
    if (!paneId) return;
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    const channel = `panes:${paneId}:line`;
    listen<PaneLine>(channel, (event) => {
      const incoming = event.payload;
      qc.setQueryData<PaneLine[]>(['panes', paneId, 'lines'], (prev = []) => {
        if (prev.some((l) => l.seq === incoming.seq)) return prev;
        return [...prev, incoming];
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
        console.warn('[usePaneLines] failed to subscribe to', channel, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [paneId, qc]);

  return query;
}

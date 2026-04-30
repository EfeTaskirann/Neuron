// `useMailbox()` — cross-pane event log per ADR-0006. Snapshot via
// `mailbox:list(null)` (full table), then `mailbox:new` events
// merge into the cache. ADR-0006 explicitly forbids a polling
// fallback: if a listener attaches AFTER an entry has already
// emitted in the same session, the snapshot covers it; if a
// session miss happens between renders, the next mount's
// snapshot will catch up.
//
// Order: most recent first (descending by ts) — matches the
// prototype's mailbox panel rendering and keeps `entries[0]` as
// the freshest message for headline displays.
import { useEffect } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { commands, type MailboxEntry } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useMailbox() {
  const qc = useQueryClient();
  const query = useQuery<MailboxEntry[]>({
    queryKey: ['mailbox'],
    queryFn: async () => {
      const list = await unwrap(commands.mailboxList(null));
      return [...list].sort((a, b) => b.ts - a.ts);
    },
  });

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<MailboxEntry>('mailbox:new', (event) => {
      const incoming = event.payload;
      qc.setQueryData<MailboxEntry[]>(['mailbox'], (prev = []) => {
        if (prev.some((e) => e.id === incoming.id)) return prev;
        // Newest at index 0 — keeps the panel order stable.
        return [incoming, ...prev];
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
        console.warn('[useMailbox] failed to subscribe to mailbox:new', err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [qc]);

  return query;
}

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
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type MailboxEntry } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { useTauriEvent } from './useTauriEvent';

export function useMailbox() {
  const qc = useQueryClient();
  const query = useQuery<MailboxEntry[]>({
    queryKey: ['mailbox'],
    queryFn: async () => {
      const list = await unwrap(commands.mailboxList(null));
      return [...list].sort((a, b) => b.ts - a.ts);
    },
  });

  useTauriEvent<MailboxEntry>('mailbox:new', (incoming) => {
    qc.setQueryData<MailboxEntry[]>(['mailbox'], (prev = []) => {
      if (prev.some((e) => e.id === incoming.id)) return prev;
      // Newest at index 0 — keeps the panel order stable.
      return [incoming, ...prev];
    });
  });

  return query;
}

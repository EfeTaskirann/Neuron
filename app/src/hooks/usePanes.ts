import { useQuery } from '@tanstack/react-query';
import { commands, type Pane } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `terminal:list` — every pane (live + closed) per the prototype's
// `data.panes` mock. Closed panes still show in the UI so the user
// can see scrollback after a process exits.
//
// `refetchInterval` — pane status transitions (running → closed/error)
// are written to SQLite by the reader/waiter without any Tauri event,
// so polling is the only way the tab strip ever reflects them
// (mirrors useAgentStatuses' polling approach).
export function usePanes() {
  return useQuery<Pane[]>({
    queryKey: ['panes'],
    queryFn: () => unwrap(commands.terminalList()),
    refetchInterval: 3000,
  });
}

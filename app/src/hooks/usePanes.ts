import { useQuery } from '@tanstack/react-query';
import { commands, type Pane } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `terminal:list` — every pane (live + closed) per the prototype's
// `data.panes` mock. Closed panes still show in the UI so the user
// can see scrollback after a process exits.
export function usePanes() {
  return useQuery<Pane[]>({
    queryKey: ['panes'],
    queryFn: () => unwrap(commands.terminalList()),
  });
}

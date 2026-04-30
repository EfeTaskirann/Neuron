import { useQuery } from '@tanstack/react-query';
import { commands, type Me } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `me:get` — current user + workspace. ADR-0005 §"shape parity":
// the hook returns the same `{ user, workspace }` mock key the
// prototype Sidebar reads, so the consumer line is the only change.
export function useMe() {
  return useQuery<Me>({
    queryKey: ['me'],
    queryFn: () => unwrap(commands.meGet()),
  });
}

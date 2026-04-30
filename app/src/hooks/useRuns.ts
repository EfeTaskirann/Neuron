import { useQuery } from '@tanstack/react-query';
import { commands, type Run, type RunFilter } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `runs:list` accepts an optional filter; the hook lets callers
// pass `undefined` (no filter) or a partial filter. The query key
// includes the filter so different filter selections cache
// separately rather than thrashing one entry.
export function useRuns(filter?: RunFilter) {
  return useQuery<Run[]>({
    queryKey: ['runs', filter ?? null],
    queryFn: () => unwrap(commands.runsList(filter ?? null)),
  });
}

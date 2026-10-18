import { useQuery } from '@tanstack/react-query';
import { commands, type AppFeatures } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

/**
 * What this run of the app offers. Fixed for the app's lifetime: the
 * backend reads `NEURON_DEV_FEATURES` once at startup.
 */
export function useAppFeatures() {
  return useQuery<AppFeatures>({
    queryKey: ['app', 'features'],
    queryFn: () => unwrap(commands.appFeatures()),
    staleTime: Infinity,
  });
}

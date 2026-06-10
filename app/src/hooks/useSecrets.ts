// Secret (API key) hooks over the `secrets:*` keychain commands
// (WP-W3-01). The backend deliberately exposes no `secrets:get` — a
// value can be written, probed for presence, or forgotten, but never
// read back across the IPC boundary — so the UI is presence-only.
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { commands } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

/** Presence probe for `key`. `true` = configured (keychain or a
 *  `NEURON_<KEY>` env override). Never carries the value. */
export function useSecretHas(key: string) {
  return useQuery<boolean>({
    queryKey: ['secret', key],
    queryFn: () => unwrap(commands.secretsHas(key)),
  });
}

/** Write `value` into the OS keychain under `key`, then refresh the
 *  presence probe so the row flips to "configured". */
export function useSecretSet() {
  const qc = useQueryClient();
  return useMutation<null, Error, { key: string; value: string }>({
    mutationFn: ({ key, value }) => unwrap(commands.secretsSet(key, value)),
    onSuccess: (_data, { key }) =>
      qc.invalidateQueries({ queryKey: ['secret', key] }),
  });
}

/** Forget (delete) the keychain entry for `key`. Idempotent. */
export function useSecretDelete() {
  const qc = useQueryClient();
  return useMutation<null, Error, string>({
    mutationFn: (key) => unwrap(commands.secretsDelete(key)),
    onSuccess: (_data, key) =>
      qc.invalidateQueries({ queryKey: ['secret', key] }),
  });
}

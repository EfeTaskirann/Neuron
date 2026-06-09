import { useEffect, useRef } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

/**
 * Subscribe to a Tauri event `channel` for the component's lifetime.
 *
 * `onEvent` receives each event's payload. The latest closure is always
 * used (held in a ref), so passing an inline handler does NOT cause a
 * resubscribe — only a `channel` change does. Pass `channel = null` to
 * suspend the subscription (e.g. behind an `enabled` flag).
 *
 * Listener registration is best-effort: a rejected `listen()` — which
 * happens under the jsdom test runtime that has no Tauri bridge — is
 * logged, not thrown. The `cancelled` guard also covers the StrictMode
 * double-mount race where the effect tears down before `listen()`
 * resolves.
 *
 * Extracted from the ~7 hooks that each hand-rolled this exact
 * listen → store-unlisten → cancelled-guarded-cleanup lifecycle.
 */
export function useTauriEvent<T>(
  channel: string | null,
  onEvent: (payload: T) => void,
): void {
  const handlerRef = useRef(onEvent);
  // Keep the ref pointing at the latest closure (updated in an effect,
  // not during render) so the long-lived subscription always calls the
  // current handler without resubscribing.
  useEffect(() => {
    handlerRef.current = onEvent;
  }, [onEvent]);

  useEffect(() => {
    if (!channel) return;
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    listen<T>(channel, (event) => handlerRef.current(event.payload))
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => {
        console.warn(`[useTauriEvent] subscribe failed: ${channel}`, err);
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [channel]);
}

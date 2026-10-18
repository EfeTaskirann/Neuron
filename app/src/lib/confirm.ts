import { ask } from '@tauri-apps/plugin-dialog';

/**
 * A warning confirmation dialog for a destructive / outward action. Wraps
 * the Tauri `ask` so every call site shares the kind and the "Vazgeç"
 * cancel label, and a dialog that fails to open does NOT perform the
 * action (returns false) — the one place this is wrong, closing the
 * window, keeps its own handler.
 */
export async function confirmWarning(opts: {
  title: string;
  message: string;
  okLabel: string;
  cancelLabel?: string;
}): Promise<boolean> {
  try {
    return await ask(opts.message, {
      title: opts.title,
      kind: 'warning',
      okLabel: opts.okLabel,
      cancelLabel: opts.cancelLabel ?? 'Vazgeç',
    });
  } catch {
    return false;
  }
}

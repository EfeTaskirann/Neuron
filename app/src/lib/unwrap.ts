import type { AppErrorWire } from './bindings';
import { appErrorCopyByKind, APP_ERROR_FALLBACK } from './copy';

// Error subclass that preserves the AppError discriminant (`kind`)
// and raw backend `detail` while exposing a Turkish user-facing
// message via `.message`. ErrorBoundary renders `.message`, so the
// display copy reaches the user without further mapping; the
// `kind`/`detail` properties stay available for any caller that
// wants to branch on variant or log the original payload.
export class AppErrorClient extends Error {
  readonly kind: string;
  readonly detail: string;
  constructor(kind: string, detail: string) {
    super(appErrorCopyByKind(kind));
    this.name = 'AppErrorClient';
    this.kind = kind;
    this.detail = detail;
  }
}

/**
 * The line to show for a failed action: the error kind's copy plus the
 * backend's detail. `AppErrorClient.message` is only the kind's generic
 * copy, so without the detail an error never says why (which folder the
 * project guard rejected, which server failed to start, …).
 */
export function describeError(err: unknown): string {
  if (err instanceof AppErrorClient && err.detail && err.detail !== err.message) {
    return `${err.message} ${err.detail}`;
  }
  return err instanceof Error ? err.message : String(err);
}

// `typedError` (in `bindings.ts`) wraps every command in a
// `{status:'ok',data}|{status:'error',error}` tagged union. TanStack
// Query expects a thrown error for the failure path, so each hook
// pipes the command through this helper.
export async function unwrap<T>(
  p: Promise<{ status: 'ok'; data: T } | { status: 'error'; error: AppErrorWire }>,
): Promise<T> {
  const r = await p;
  if (r.status === 'ok') return r.data;
  const err = r.error as { kind?: string; message?: string };
  if (typeof err.kind === 'string') {
    throw new AppErrorClient(err.kind, err.message ?? '');
  }
  throw new Error(err.message ?? APP_ERROR_FALLBACK);
}

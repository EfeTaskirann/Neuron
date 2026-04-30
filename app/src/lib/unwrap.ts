import type { AppErrorWire } from './bindings';

// `typedError` (in `bindings.ts`) wraps every command in a
// `{status:'ok',data}|{status:'error',error}` tagged union. TanStack
// Query expects a thrown error for the failure path, so each hook
// pipes the command through this helper.
export async function unwrap<T>(
  p: Promise<{ status: 'ok'; data: T } | { status: 'error'; error: AppErrorWire }>,
): Promise<T> {
  const r = await p;
  if (r.status === 'ok') return r.data;
  // `AppErrorWire` carries a discriminant (`kind`) and a human
  // `message`; surface the message because that's what
  // ErrorBoundary will render. Fall back to the kind so we never
  // throw an empty-string error.
  const err = r.error as { kind?: string; message?: string };
  throw new Error(err.message ?? err.kind ?? 'Backend command failed');
}

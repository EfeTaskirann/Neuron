// Tiny toast system: module-level subscribable store so non-component
// code (mutation hooks, error handlers) can dispatch without
// prop-drilling a React context. `<ToastHost />` mounts once near the
// root and renders whatever the store currently holds.

export type ToastVariant = 'info' | 'success' | 'error';

export interface Toast {
  id: number;
  variant: ToastVariant;
  body: string;
  // `null` = persistent (caller dismisses by id); number in ms = auto-dismiss.
  durationMs: number | null;
}

type Listener = (toasts: readonly Toast[]) => void;

let nextId = 1;
let toasts: Toast[] = [];
const listeners = new Set<Listener>();
const timers = new Map<number, ReturnType<typeof setTimeout>>();

function emit(): void {
  for (const fn of listeners) fn(toasts);
}

export function subscribeToasts(fn: Listener): () => void {
  listeners.add(fn);
  fn(toasts);
  return () => {
    listeners.delete(fn);
  };
}

export function showToast(input: Omit<Toast, 'id'>): number {
  const id = nextId++;
  toasts = [...toasts, { id, ...input }];
  if (input.durationMs !== null) {
    const t = setTimeout(() => dismissToast(id), input.durationMs);
    timers.set(id, t);
  }
  emit();
  return id;
}

export function dismissToast(id: number): void {
  const timer = timers.get(id);
  if (timer) {
    clearTimeout(timer);
    timers.delete(id);
  }
  toasts = toasts.filter((t) => t.id !== id);
  emit();
}

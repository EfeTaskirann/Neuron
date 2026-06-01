import { useSyncExternalStore } from 'react';

// Client-side "active project" state. The app is gated behind a
// Pick-Project landing screen (see ProjectPickerRoute) until this
// store holds a project — once set, every tab in the navigation
// becomes accessible. The picked folder is persisted to
// localStorage so the user picks once per machine, not once per
// session.
//
// Mirrors `useAppearance.ts`'s `useSyncExternalStore` pattern: a
// single module-level snapshot + a Set of subscribers + a
// localStorage-backed write through. The shared snapshot means
// every consumer (App gate, Topbar chip, TerminalSwarm cwd, new-
// shell default cwd) reads the same value without prop drilling.

export interface ActiveProject {
  /** Absolute path to the project root, exactly as returned by the
   *  Tauri folder picker dialog. Used verbatim as the `cwd` for
   *  spawn commands. */
  path: string;
  /** Last segment of the path — what the user sees in the header
   *  chip. Computed at pick time and stored so we don't have to
   *  reparse on every render. */
  name: string;
  /** Wall-clock timestamp (ms since epoch) of when the project was
   *  picked. Powers the "recent projects" ordering when we wire
   *  that up; harmless if unused today. */
  pickedAt: number;
}

const STORAGE_KEY = 'neuron.activeProject';

function deriveName(path: string): string {
  // Last path segment on both POSIX and Windows. The dialog returns
  // platform-native separators, so split on both `/` and `\`.
  const trimmed = path.replace(/[\\/]+$/, '');
  const idx = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
}

function read(): ActiveProject | null {
  if (typeof localStorage === 'undefined') return null;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<ActiveProject>;
    if (typeof parsed.path !== 'string' || parsed.path.length === 0) {
      return null;
    }
    return {
      path: parsed.path,
      name:
        typeof parsed.name === 'string' && parsed.name.length > 0
          ? parsed.name
          : deriveName(parsed.path),
      pickedAt:
        typeof parsed.pickedAt === 'number' ? parsed.pickedAt : Date.now(),
    };
  } catch {
    return null;
  }
}

let current: ActiveProject | null = read();
const listeners = new Set<() => void>();

function emit(): void {
  for (const l of listeners) l();
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

function snapshot(): ActiveProject | null {
  return current;
}

function writeStorage(next: ActiveProject | null): void {
  if (typeof localStorage === 'undefined') return;
  try {
    if (next == null) {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    }
  } catch {
    // Ignore — private-mode WebViews can throw; the in-memory
    // snapshot is the source of truth for the running session.
  }
}

export function setActiveProject(path: string): void {
  const next: ActiveProject = {
    path,
    name: deriveName(path),
    pickedAt: Date.now(),
  };
  current = next;
  writeStorage(next);
  emit();
}

export function clearActiveProject(): void {
  current = null;
  writeStorage(null);
  emit();
}

export interface UseActiveProjectResult {
  project: ActiveProject | null;
  setProject: (path: string) => void;
  clearProject: () => void;
}

export function useActiveProject(): UseActiveProjectResult {
  const project = useSyncExternalStore(subscribe, snapshot, snapshot);
  return {
    project,
    setProject: setActiveProject,
    clearProject: clearActiveProject,
  };
}

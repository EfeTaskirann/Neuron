import { useSyncExternalStore } from 'react';

// Client-side appearance state. SettingsRoute used to render
// presentational stubs with hardcoded "active" classes; this hook
// now owns theme/accent/density/motion and persists them to
// localStorage. Backend persistence (the planned Tweaks pipeline)
// can later read from / write to this same shape — see ADR notes
// in PROJECT_CHARTER.md.

export type Theme = 'light' | 'dark' | 'system';
export type Density = 'comfortable' | 'compact';
export type Motion = 'full' | 'reduced' | 'off';

export interface Appearance {
  theme: Theme;
  accent: string;
  density: Density;
  motion: Motion;
}

// Hex stays here so the swatch row in SettingsRoute and the
// CSS-variable derivation in `apply()` agree on the canonical set.
export const ACCENT_SWATCHES: readonly string[] = [
  '#a874d6',
  '#7aa6f0',
  '#e0a85b',
  '#7ad6c8',
  '#d678a6',
];

const DEFAULTS: Appearance = {
  theme: 'dark',
  accent: '#a874d6',
  density: 'comfortable',
  motion: 'full',
};

const STORAGE_KEY = 'neuron.appearance';

function read(): Appearance {
  if (typeof localStorage === 'undefined') return DEFAULTS;
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return DEFAULTS;
    const parsed = JSON.parse(raw) as Partial<Appearance>;
    return { ...DEFAULTS, ...parsed };
  } catch {
    return DEFAULTS;
  }
}

let current: Appearance = read();
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

function snapshot(): Appearance {
  return current;
}

function update(next: Partial<Appearance>): void {
  current = { ...current, ...next };
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
  } catch {
    // Ignore — quota or disabled storage shouldn't break the UI.
  }
  apply(current);
  emit();
}

function resolveTheme(theme: Theme): 'light' | 'dark' {
  if (theme !== 'system') return theme;
  if (typeof window === 'undefined' || !window.matchMedia) return 'dark';
  return window.matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light';
}

// Drives the `<html>` element: theme class/attribute, density and
// motion data-attributes, and accent-derived CSS variables. Kept
// idempotent so module-load and post-mutation calls both converge.
export function apply(a: Appearance = current): void {
  if (typeof document === 'undefined') return;
  const html = document.documentElement;
  const resolved = resolveTheme(a.theme);

  html.classList.toggle('dark', resolved === 'dark');
  html.setAttribute('data-theme', resolved);
  html.setAttribute('data-density', a.density);
  html.setAttribute('data-motion', a.motion);

  // Override the violet palette with shades derived from the chosen
  // swatch so gradients/glows that reference --neuron-violet-* stay
  // coherent instead of flattening to a single tone.
  const s = a.accent;
  html.style.setProperty('--neuron-violet-300', `color-mix(in oklch, ${s} 60%, white)`);
  html.style.setProperty('--neuron-violet-400', `color-mix(in oklch, ${s} 80%, white)`);
  html.style.setProperty('--neuron-violet-500', s);
  html.style.setProperty('--neuron-violet-600', `color-mix(in oklch, ${s} 85%, black)`);
  html.style.setProperty('--neuron-violet-700', `color-mix(in oklch, ${s} 70%, black)`);
  html.style.setProperty('--ring', s);
  html.style.setProperty(
    '--primary',
    resolved === 'dark' ? s : `color-mix(in oklch, ${s} 85%, black)`,
  );

  const meta = document.querySelector('meta[name="color-scheme"]');
  if (meta) meta.setAttribute('content', resolved);
}

// Reapply when the OS theme flips while "system" is selected.
if (typeof window !== 'undefined' && window.matchMedia) {
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const onChange = (): void => {
    if (current.theme === 'system') apply(current);
  };
  if (typeof mq.addEventListener === 'function') {
    mq.addEventListener('change', onChange);
  }
}

// Boot — make sure the DOM reflects persisted preferences before
// the first React render so we don't flash the default look.
apply(current);

export interface UseAppearanceResult extends Appearance {
  setTheme: (t: Theme) => void;
  setAccent: (a: string) => void;
  setDensity: (d: Density) => void;
  setMotion: (m: Motion) => void;
}

export function useAppearance(): UseAppearanceResult {
  const state = useSyncExternalStore(subscribe, snapshot, snapshot);
  return {
    ...state,
    setTheme: (t) => update({ theme: t }),
    setAccent: (a) => update({ accent: a }),
    setDensity: (d) => update({ density: d }),
    setMotion: (m) => update({ motion: m }),
  };
}

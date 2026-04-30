// Inline icon system ported from `Neuron Design/app/icons.jsx`.
// Lucide-style 24×24 viewBox, stroke 1.75, currentColor — same paths
// as the prototype so all CSS rules and the surrounding DOM keep
// working unchanged. Window globals are gone; consumers import
// `NIcon`, `NodeGlyph`, `Brandmark` directly.
import type { CSSProperties, ReactNode } from 'react';

export type IconName =
  | 'workflow' | 'terminal' | 'server' | 'bot' | 'plug'
  | 'activity' | 'store' | 'settings'
  | 'search' | 'plus'
  | 'chevron' | 'chevronD' | 'chevronR' | 'chevronL' | 'chevronU'
  | 'close' | 'check'
  | 'sparkles' | 'wrench' | 'branch' | 'hand' | 'zap' | 'layers' | 'cube'
  | 'sun' | 'moon'
  | 'play' | 'pause' | 'stop' | 'refresh' | 'clock'
  | 'copy' | 'trash' | 'star' | 'download' | 'upload' | 'link' | 'eye' | 'filter' | 'more'
  | 'info' | 'alert' | 'error'
  | 'arrowUp' | 'arrowDown' | 'arrowR' | 'arrowL';

const ICON_PATHS: Record<IconName, ReactNode> = {
  workflow: (
    <>
      <rect x="3" y="3" width="6" height="6" rx="1.5" />
      <rect x="15" y="3" width="6" height="6" rx="1.5" />
      <rect x="15" y="15" width="6" height="6" rx="1.5" />
      <path d="M9 6h6 M18 9v6 M15 18H9a3 3 0 0 1-3-3V9" />
    </>
  ),
  terminal: (
    <>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M7 9l3 3-3 3 M13 15h4" />
    </>
  ),
  server: (
    <>
      <rect x="3" y="4" width="18" height="6" rx="1.5" />
      <rect x="3" y="14" width="18" height="6" rx="1.5" />
      <path d="M7 7h.01 M7 17h.01" />
    </>
  ),
  bot: (
    <>
      <rect x="4" y="8" width="16" height="12" rx="2.5" />
      <path d="M12 8V4 M9 14h.01 M15 14h.01 M9 18h6" />
      <path d="M2 13v3 M22 13v3" />
    </>
  ),
  plug: <path d="M9 7V2 M15 7V2 M5 13l4 9h6l4-9V7H5z M12 22v-5" />,
  activity: <path d="M22 12h-4l-3 9L9 3l-3 9H2" />,
  store: (
    <>
      <path d="M3 9l1-5h16l1 5" />
      <path d="M4 9v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9" />
      <path d="M9 22V12h6v10" />
    </>
  ),
  settings: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h.1a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v.1a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
    </>
  ),
  search: (<><circle cx="11" cy="11" r="7" /><path d="M21 21l-4.3-4.3" /></>),
  plus: <path d="M12 5v14 M5 12h14" />,
  chevron: <path d="M6 9l6 6 6-6" />,
  chevronD: <path d="M6 9l6 6 6-6" />,
  chevronR: <path d="M9 6l6 6-6 6" />,
  chevronL: <path d="M15 6l-6 6 6 6" />,
  chevronU: <path d="M6 15l6-6 6 6" />,
  close: <path d="M18 6L6 18 M6 6l12 12" />,
  check: <path d="M5 12l5 5L20 7" />,
  sparkles: (
    <>
      <path d="M12 3l1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5z" />
      <path d="M19 14l.7 2.1L22 17l-2.1.7L19 20l-.7-2.1L16 17l2.3-.7z" />
    </>
  ),
  wrench: <path d="M14.5 6.5L18 3l3 3-3.5 3.5h-2l-8.5 8.5L4 21l-3-3 5-5 2 2 8.5-8.5z" />,
  branch: (
    <>
      <circle cx="6" cy="3" r="2" />
      <circle cx="6" cy="18" r="2" />
      <circle cx="18" cy="6" r="2" />
      <path d="M6 5v8a3 3 0 0 0 3 3h6 M16 7a3 3 0 0 1-3 3h-3" />
    </>
  ),
  hand: <path d="M9 13V5a1.2 1.2 0 0 1 2.4 0 M11.4 4a1.2 1.2 0 0 1 2.4 0v7 M13.8 5.5a1.2 1.2 0 0 1 2.4 0v6 M16.2 8a1.2 1.2 0 0 1 2.4 0v7a6 6 0 0 1-11 3l-2.1-4.5a1.3 1.3 0 0 1 1.8-1.8L9 13" />,
  zap: <path d="M13 2L3 14h7l-1 8 10-12h-7l1-8z" />,
  layers: (
    <>
      <path d="M12 2L2 7l10 5 10-5-10-5z" />
      <path d="M2 12l10 5 10-5 M2 17l10 5 10-5" />
    </>
  ),
  cube: (
    <>
      <path d="M12 2L3 7v10l9 5 9-5V7l-9-5z" />
      <path d="M3 7l9 5 9-5 M12 12v10" />
    </>
  ),
  sun: (
    <>
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4" />
    </>
  ),
  moon: <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />,
  play: <path d="M5 3l14 9-14 9V3z" fill="currentColor" />,
  pause: (
    <>
      <rect x="6" y="4" width="4" height="16" rx="1" />
      <rect x="14" y="4" width="4" height="16" rx="1" />
    </>
  ),
  stop: <rect x="5" y="5" width="14" height="14" rx="2" />,
  refresh: <path d="M3 12a9 9 0 0 1 15.5-6.4L21 8 M21 3v5h-5 M21 12a9 9 0 0 1-15.5 6.4L3 16 M3 21v-5h5" />,
  clock: (<><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>),
  copy: (
    <>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
    </>
  ),
  trash: (
    <path d="M3 6h18 M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2 M6 6l1 14a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-14" />
  ),
  star: <path d="M12 2l3 7 7 1-5 5 1 7-6-3-6 3 1-7-5-5 7-1z" />,
  download: <path d="M12 3v12 M7 10l5 5 5-5 M5 21h14" />,
  upload: <path d="M12 21V9 M7 14l5-5 5 5 M5 3h14" />,
  link: <path d="M10 13a5 5 0 0 0 7 0l3-3a5 5 0 0 0-7-7l-1 1 M14 11a5 5 0 0 0-7 0l-3 3a5 5 0 0 0 7 7l1-1" />,
  eye: (<><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z" /><circle cx="12" cy="12" r="3" /></>),
  filter: <path d="M3 5h18l-7 9v6l-4-2v-4z" />,
  more: (
    <>
      <circle cx="6" cy="12" r="1.4" />
      <circle cx="12" cy="12" r="1.4" />
      <circle cx="18" cy="12" r="1.4" />
    </>
  ),
  info: (<><circle cx="12" cy="12" r="9" /><path d="M12 8h.01 M11 12h1v5h1" /></>),
  alert: (<><path d="M12 3l10 17H2L12 3z" /><path d="M12 10v4 M12 17h.01" /></>),
  error: (<><circle cx="12" cy="12" r="9" /><path d="M9 9l6 6 M15 9l-6 6" /></>),
  arrowUp: <path d="M12 19V5 M5 12l7-7 7 7" />,
  arrowDown: <path d="M12 5v14 M5 12l7 7 7-7" />,
  arrowR: <path d="M5 12h14 M13 5l7 7-7 7" />,
  arrowL: <path d="M19 12H5 M11 5l-7 7 7 7" />,
};

export interface NIconProps {
  name: IconName;
  size?: number;
  stroke?: number;
  color?: string;
  style?: CSSProperties;
  className?: string;
}

export function NIcon({
  name,
  size = 16,
  stroke = 1.75,
  color = 'currentColor',
  style,
  className = '',
}: NIconProps): JSX.Element {
  return (
    <svg
      className={`n-icon n-icon-${name} ${className}`.trim()}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={stroke}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flex: 'none', ...style }}
      aria-hidden="true"
    >
      {ICON_PATHS[name]}
    </svg>
  );
}

export type NodeKind = 'llm' | 'tool' | 'logic' | 'human' | 'mcp';

const KIND_TINT: Record<NodeKind, { color: string; bg: string; border: string }> = {
  llm: {
    color: 'var(--neuron-violet-300)',
    bg: 'color-mix(in oklch, var(--neuron-violet-500) 18%, transparent)',
    border: 'color-mix(in oklch, var(--neuron-violet-400) 55%, var(--border))',
  },
  tool: {
    color: 'var(--neuron-sky-300)',
    bg: 'color-mix(in oklch, var(--neuron-sky-500) 18%, transparent)',
    border: 'color-mix(in oklch, var(--neuron-sky-400) 55%, var(--border))',
  },
  logic: {
    color: 'var(--neuron-slate-300)',
    bg: 'color-mix(in oklch, var(--neuron-slate-500) 22%, transparent)',
    border: 'color-mix(in oklch, var(--neuron-slate-400) 55%, var(--border))',
  },
  human: {
    color: 'var(--neuron-amber-300)',
    bg: 'color-mix(in oklch, var(--neuron-amber-500) 16%, transparent)',
    border: 'color-mix(in oklch, var(--neuron-amber-400) 55%, var(--border))',
  },
  mcp: {
    color: 'var(--neuron-violet-200)',
    bg: 'linear-gradient(135deg, var(--neuron-midnight-800), var(--neuron-midnight-950))',
    border: 'color-mix(in oklch, var(--neuron-violet-500) 35%, var(--border))',
  },
};

const KIND_GLYPH: Record<NodeKind, ReactNode> = {
  llm: (
    <>
      <circle cx="12" cy="12" r="2.4" />
      <circle cx="12" cy="12" r="6.5" opacity="0.8" />
      <circle cx="12" cy="12" r="9.5" opacity="0.45" />
    </>
  ),
  tool: <path d="M14.5 6.5L18 3l3 3-3.5 3.5h-2l-8.5 8.5L4 21l-3-3 5-5 2 2 8.5-8.5z" />,
  logic: (
    <>
      <circle cx="6" cy="3" r="1.8" />
      <circle cx="6" cy="18" r="1.8" />
      <circle cx="18" cy="6" r="1.8" />
      <path d="M6 5v8a3 3 0 0 0 3 3h6 M16 7a3 3 0 0 1-3 3h-3" />
    </>
  ),
  human: (
    <>
      <circle cx="12" cy="6" r="2.5" />
      <path d="M5 21c0-4 3-7 7-7s7 3 7 7" />
    </>
  ),
  mcp: (
    <>
      <path d="M12 2L3 7v10l9 5 9-5V7l-9-5z" />
      <path d="M3 7l9 5 9-5 M12 12v10" />
    </>
  ),
};

export interface NodeGlyphProps {
  kind?: NodeKind;
  size?: number;
}

export function NodeGlyph({ kind = 'llm', size = 24 }: NodeGlyphProps): JSX.Element {
  const tint = KIND_TINT[kind];
  const glyph = KIND_GLYPH[kind];
  const inner = Math.round(size * 0.62);
  return (
    <span
      className={`node-glyph node-glyph-${kind}`}
      style={{
        width: size,
        height: size,
        borderRadius: Math.max(6, Math.round(size * 0.28)),
        background: tint.bg,
        border: `1px solid ${tint.border}`,
        display: 'grid',
        placeItems: 'center',
        flex: 'none',
        color: tint.color,
      }}
      aria-hidden="true"
    >
      <svg
        width={inner}
        height={inner}
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {glyph}
      </svg>
    </span>
  );
}

export interface BrandmarkProps {
  size?: number;
}

export function Brandmark({ size = 28 }: BrandmarkProps): JSX.Element {
  const id = `bm-${size}`;
  return (
    <svg
      className="n-brandmark"
      width={size}
      height={size}
      viewBox="0 0 64 64"
      fill="none"
      aria-hidden="true"
    >
      <defs>
        <linearGradient id={id} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="oklch(0.823 0.103 301)" />
          <stop offset="1" stopColor="oklch(0.470 0.207 296)" />
        </linearGradient>
        <radialGradient id={`${id}-glow`} cx="0.5" cy="0.5" r="0.5">
          <stop offset="0" stopColor="oklch(0.643 0.214 298 / 0.5)" />
          <stop offset="1" stopColor="oklch(0.643 0.214 298 / 0)" />
        </radialGradient>
      </defs>
      <rect width="64" height="64" rx="14" fill="oklch(0.190 0.046 258)" />
      <rect width="64" height="64" rx="14" fill={`url(#${id}-glow)`} />
      <circle cx="20" cy="22" r="5.5" fill={`url(#${id})`} />
      <path
        d="M24 26 C 30 32, 34 38, 38 42"
        stroke={`url(#${id})`}
        strokeWidth="2.6"
        strokeLinecap="round"
        fill="none"
      />
      <circle cx="42" cy="44" r="6" fill="none" stroke={`url(#${id})`} strokeWidth="2.6" />
      <circle cx="48" cy="22" r="2.6" fill={`url(#${id})`} opacity="0.85" />
    </svg>
  );
}

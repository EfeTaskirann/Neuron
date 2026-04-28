/* global React */
const { useState } = React;

// ============== Icons (inline so we don't depend on CDN) ==============
const Icon = ({ name, size = 16, color = "currentColor", strokeWidth = 1.75 }) => {
  const paths = {
    workflow: <><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="15" y="15" width="6" height="6" rx="1.5"/><rect x="15" y="3" width="6" height="6" rx="1.5"/><path d="M9 6h6M18 9v6M15 18H9a3 3 0 0 1-3-3V9"/></>,
    bot: <><rect x="3" y="8" width="18" height="12" rx="3"/><path d="M12 8V4M9 12h.01M15 12h.01M9 16h6"/></>,
    plug: <><path d="M12 22v-5M9 7V2M15 7V2M5 13l4 9h6l4-9V7H5z"/></>,
    activity: <><path d="M22 12h-4l-3 9L9 3l-3 9H2"/></>,
    store: <><path d="M3 9l1-5h16l1 5M4 9v11a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1V9M9 22V12h6v10"/></>,
    settings: <><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1z"/></>,
    search: <><circle cx="11" cy="11" r="7"/><path d="M21 21l-4.3-4.3"/></>,
    plus: <><path d="M12 5v14M5 12h14"/></>,
    chevron: <><path d="M6 9l6 6 6-6"/></>,
    sparkles: <><path d="M12 3l1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5z"/><path d="M19 14l.7 2.1L22 17l-2.1.7L19 20l-.7-2.1L16 17l2.3-.7z"/></>,
    wrench: <><path d="M14.5 6.5L18 3l3 3-3.5 3.5-2 0-8.5 8.5L4 21l-3-3 5-5 2 2 8.5-8.5z"/></>,
    branch: <><circle cx="6" cy="3" r="2"/><circle cx="6" cy="18" r="2"/><circle cx="18" cy="6" r="2"/><path d="M6 5v8a3 3 0 0 0 3 3h6M16 7a3 3 0 0 1-3 3h-3"/></>,
    hand: <><path d="M9 13V5a1.2 1.2 0 0 1 2.4 0M11.4 4a1.2 1.2 0 0 1 2.4 0v7M13.8 5.5a1.2 1.2 0 0 1 2.4 0v6M16.2 8a1.2 1.2 0 0 1 2.4 0v7a6 6 0 0 1-11 3l-2.1-4.5a1.3 1.3 0 0 1 1.8-1.8L9 13"/></>,
    server: <><path d="M4 8L12 4L20 8L20 16L12 20L4 16Z"/><path d="M4 8L12 12L20 8M12 12V20"/></>,
    sun: <><circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"/></>,
    moon: <><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z"/></>,
    play: <><path d="M5 3l14 9-14 9V3z" fill="currentColor"/></>,
    close: <><path d="M18 6L6 18M6 6l12 12"/></>,
  };
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={color} strokeWidth={strokeWidth} strokeLinecap="round" strokeLinejoin="round">
      {paths[name] || null}
    </svg>
  );
};

// ============== Primitives ==============
const Button = ({ variant = "primary", size = "md", children, leftIcon, rightIcon, onClick, style }) => {
  const sizes = { sm: { h: 28, px: 10, fs: 12 }, md: { h: 36, px: 14, fs: 13 }, lg: { h: 44, px: 18, fs: 14 } };
  const s = sizes[size];
  const variants = {
    primary: { background: "var(--primary)", color: "var(--primary-foreground)", border: "1px solid transparent" },
    secondary: { background: "var(--secondary)", color: "var(--secondary-foreground)", border: "1px solid transparent" },
    outline: { background: "transparent", color: "var(--foreground)", border: "1px solid var(--border)" },
    ghost: { background: "transparent", color: "var(--foreground)", border: "1px solid transparent" },
    destructive: { background: "var(--destructive)", color: "var(--destructive-foreground)", border: "1px solid transparent" },
  };
  const [hover, setHover] = useState(false);
  const hoverGlow = variant === "primary" && hover ? { boxShadow: "var(--glow-violet-sm)", transform: "translateY(-1px)" } : {};
  return (
    <button
      onClick={onClick}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      style={{
        height: s.h, padding: `0 ${s.px}px`, fontSize: s.fs, fontWeight: 500,
        fontFamily: "var(--font-sans)", borderRadius: 8, cursor: "pointer",
        display: "inline-flex", alignItems: "center", gap: 6,
        transition: "all 120ms var(--ease-out)",
        ...variants[variant], ...hoverGlow, ...style,
      }}
    >
      {leftIcon && <Icon name={leftIcon} size={14} />}
      {children}
      {rightIcon && <Icon name={rightIcon} size={14} />}
    </button>
  );
};

const Badge = ({ children, variant = "default", style }) => {
  const variants = {
    default: { background: "var(--secondary)", color: "var(--secondary-foreground)" },
    outline: { background: "transparent", color: "var(--muted-foreground)", border: "1px solid var(--border)" },
    featured: { background: "linear-gradient(135deg, var(--neuron-violet-600), var(--neuron-violet-800))", color: "#fff" },
  };
  return (
    <span style={{
      display: "inline-flex", alignItems: "center", gap: 6,
      fontSize: 11, fontWeight: 500, padding: "2px 8px",
      borderRadius: variant === "featured" ? 9999 : 4,
      ...variants[variant], ...style,
    }}>{children}</span>
  );
};

const StatusDot = ({ variant = "online", size = 8 }) => {
  const colors = {
    online:   { bg: "var(--neuron-emerald-500)", glow: "var(--glow-emerald-sm)" },
    success:  { bg: "var(--neuron-emerald-500)", glow: "var(--glow-emerald-sm)" },
    degraded: { bg: "var(--neuron-amber-500)" },
    warning:  { bg: "var(--neuron-amber-500)" },
    offline:  { bg: "var(--neuron-slate-500)" },
    idle:     { bg: "var(--neuron-slate-500)" },
    error:    { bg: "var(--neuron-rose-500)" },
    running:  { bg: "var(--neuron-violet-400)", glow: "var(--glow-violet-sm)" },
  };
  const c = colors[variant] || colors.idle;
  return <span style={{ width: size, height: size, borderRadius: 9999, background: c.bg, boxShadow: c.glow, flex: "none" }} />;
};

const Kbd = ({ children }) => (
  <span style={{
    fontFamily: "var(--font-mono)", fontSize: 10, padding: "2px 6px",
    borderRadius: 4, background: "oklch(1 0 0 / 0.08)",
    border: "1px solid oklch(1 0 0 / 0.1)", color: "var(--muted-foreground)",
  }}>{children}</span>
);

const Brandmark = ({ size = 28 }) => (
  <svg width={size} height={size} viewBox="0 0 64 64" fill="none">
    <defs>
      <linearGradient id="bm" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stopColor="#c4a1e4"/><stop offset="1" stopColor="#5d2596"/>
      </linearGradient>
    </defs>
    <rect width="64" height="64" rx="14" fill="oklch(0.190 0.046 258)"/>
    <circle cx="22" cy="24" r="5.5" fill="url(#bm)"/>
    <path d="M26 28 C 32 35, 36 39, 40 42" stroke="url(#bm)" strokeWidth="2.4" strokeLinecap="round" fill="none"/>
    <circle cx="42" cy="44" r="5.5" fill="none" stroke="url(#bm)" strokeWidth="2.4"/>
  </svg>
);

window.NeuronUI = { Icon, Button, Badge, StatusDot, Kbd, Brandmark };

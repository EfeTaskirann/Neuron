# Neuron Design System — Specification

**Version:** 1.0
**Owner:** Efe Taşkıran
**Last updated:** 2026-04-28
**Status:** Authoritative for all UI work

This is the consolidated source of truth for visual decisions. Where this file conflicts with `Neuron Design/README.md` or `Neuron Design/SKILL.md`, this file wins. Those files are reference-only and will be deleted in WP-W2-08.

## 1. Brand

Neuron is a desktop **Agent Development Environment (ADE)** for multi-agent workflow building, MCP server management, and observability. It positions alongside premium-consumer dev tools (**Arc Browser**, **Things 3**) — not the sterile monochrome of Linear/Vercel.

The brand metaphor is *neurons*: nodes-and-synapses, soft glow feedback, signal propagation. A workflow canvas is a network of firing neurons. Status changes "feel electric."

## 2. Voice

- **Tone:** Calm, precise, a little reverent of the craft. Things 3 ("Get things done.") meets Linear's product-spec terseness.
- **Sentences:** Short. Verbs concrete (*build, observe, orchestrate*).
- **Avoid:** AI-industry buzzwords ("intelligent", "next-gen", "AI-powered"), exclamation marks, anthropomorphism ("your AI assistant").
- **Casing:** Sentence case in UI. Title Case for proper-noun product surfaces ("MCP Marketplace", "Run Inspector"). UPPERCASE only for `.text-overline` (tracking 0.08em).
- **Pronouns:** Implicit "you" in CTAs ("Create workflow"). Never "I/we" in product copy.
- **Bilingual:** Primary English, full Turkish parity. Settings exposes EN/TR toggle.

### Examples

| Surface | EN | TR |
|---|---|---|
| Tagline | "Where agents connect." | "Ajanların buluştuğu yer." |
| Empty state | "No runs yet — start a workflow to see traces here." | "Henüz çalıştırma yok — bir akış başlat ki izler burada görünsün." |
| Toast | "Saved" / "Connection failed — check your API key" | "Kaydedildi" / "Bağlantı başarısız — API anahtarını kontrol et" |
| Button | "New workflow" / "Install" / "Connect server" | "Yeni akış" / "Yükle" / "Sunucuyu bağla" |

## 3. Color (OKLCH only)

### Primitive palettes

- **Violet** (hue 295) — primary action, active state. Anchor: `--neuron-violet-500` `oklch(0.643 0.214 298)`.
- **Midnight** (hue 258) — surface and dark-mode background. Base: `--neuron-midnight-950` `oklch(0.135 0.032 258)`.
- **Status** — emerald (success), amber (warning / human-in-the-loop), rose (destructive), sky (tool-node), slate (logic-node neutral).

Primitive scales (50, 100, ..., 950) live in `colors_and_type.css`. **Never use primitives directly in UI**; reference semantic tokens.

### Semantic tokens (Tier 2)

`--background`, `--foreground`, `--card`, `--popover`, `--primary`, `--secondary`, `--muted`, `--muted-foreground`, `--accent`, `--destructive`, `--border`, `--input`, `--ring`, plus `--surface-1/2/3` for elevation tiers.

### Synaptic semantic aliases (status-stable)

`--syn-running` (violet), `--syn-success` (emerald), `--syn-error` (rose), `--syn-warning` (amber), `--syn-info` (sky). These bypass the accent swap so status colors remain stable when the user picks a different accent hue.

### Mode

Default mode is **dark** (`html class="dark"`). Light is supported, near-white with a hint of violet (not pure white).

### Anti-patterns

- ❌ HSL or hex literals in production CSS (legacy SVGs may keep them)
- ❌ Custom one-off colors not derived from primitives
- ❌ `color-mix()` between two unrelated palettes (only same-palette mixes)

## 4. Typography

- **Sans:** Geist Sans (self-hosted), Inter fallback
- **Display:** Geist (900 weight)
- **Mono:** Geist Mono, JetBrains Mono fallback

Modular scale 1.200, 16px base. Tracking tightens with size:

| Token | Size | Line-height | Tracking | Weight |
|---|---|---|---|---|
| `display` | 48 | 1.05 | -0.02em | 900 |
| `h1` | 32 | 1.15 | -0.015em | 900 |
| `h2` | 24 | 1.20 | -0.01em | 600 |
| `h3` | 20 | 1.25 | -0.005em | 600 |
| `h4` | 16 | 1.30 | 0 | 600 |
| `body-lg` | 16 | 1.55 | 0 | 400 |
| `body` | 14 | 1.50 | 0 | 400 |
| `body-sm` | 13 | 1.45 | 0 | 400 |
| `caption` | 12 | 1.40 | 0.01em | 500 |
| `overline` | 11 | 1.30 | 0.08em (UPPERCASE) | 600 |
| `code` | 13 | 1.55 | 0 | 400 (mono) |

Variable fonts mandatory (animated weight transitions allowed).

## 5. Spacing

4px base grid. Named semantic spacings:

| Token | px |
|---|---|
| `--space-1` | 4 |
| `--space-2` | 8 |
| `--space-3` | 12 |
| `--space-4` | 16 |
| `--space-6` | 24 (gutter) |
| `--space-8` | 32 |
| `--space-12` | 48 (section) |
| `--space-stack-sm/md/lg` | 8 / 16 / 32 |

Sidebar: 64 collapsed / 248 expanded / 300 wide. Inspector: 320–520 (default 400). Modal max-w-lg. Touch target minimum 40px (Things 3 hizası), critical actions 44px.

## 6. Radii

| Token | px | Used by |
|---|---|---|
| `--radius-xs` | 2 | Dot indicators |
| `--radius-sm` | 4 | Badges |
| `--radius-md` | 8 | Inputs, default buttons |
| `--radius-lg` | 12 | Cards (default) |
| `--radius-xl` | 16 | Canvas nodes, modals |
| `--radius-2xl` | 24 | Marketplace cards |
| `--radius-full` | 9999 | Pills, dots, avatars |

Cap at 24. **No Material pill buttons.**

## 7. Shadows & glow

All shadows are violet-tinted at very low opacity (large blur, `oklch(midnight-950 / 6–22%)`). Never sharp black.

- `--shadow-xs/sm/md/lg/xl` — elevation tiers
- `--shadow-card` (alias `var(--shadow-sm)`), `--shadow-card-hover` (alias `--shadow-md`), `--shadow-pop` (alias `--shadow-lg`)

**Synaptic glow** is the brand signature; reserved for **only**:
1. Hovered primary CTA (`var(--glow-violet-sm)`)
2. Selected canvas node (`var(--glow-violet-md)`)
3. Running span / running pane (`var(--glow-violet-sm)`)
4. Focus ring (keyboard, `var(--glow-focus-sm)`)

`--glow-emerald-sm`, `--glow-amber-sm`, `--glow-rose-sm` exist for status pills but are sparingly used.

## 8. Motion

Library: `motion` (motion/react v12+), **NOT framer-motion**. Tokens:

| Token | Value | Used for |
|---|---|---|
| `--ease-out` | `cubic-bezier(0.16, 1, 0.3, 1)` | Apple-vari |
| `--ease-out-soft` | `cubic-bezier(0.22, 1, 0.36, 1)` | Things-vari |
| `--ease-spring` | `cubic-bezier(0.34, 1.56, 0.64, 1)` | Arc-vari light overshoot |

| Token | ms |
|---|---|
| `--duration-fast` | 120 |
| `--duration-normal` | 200 |
| `--duration-moderate` | 300 |
| `--duration-slow` | 400 |
| `--duration-slower` | 600 |

Signature patterns:
- Hover-lift `translateY(-1px)` + `shadow-md` 120ms
- Press scale 0.98 80ms
- Modal enter spring 300ms
- Node pulse on running status (violet glow oscillation 1600ms)
- Edge dataflow (dashed stroke offset animation 600ms linear infinite)
- Span shimmer (sheen sweep 1600ms linear infinite)

`prefers-reduced-motion: reduce` → all animations cap at 1ms iteration count, transitions cap at 150ms.

## 9. Cards

- Background: `var(--card)`
- Radius: `var(--radius-lg)` or `var(--radius-xl)`
- Default shadow: `var(--shadow-card)`
- Hover: `var(--shadow-card-hover)` + `translateY(-1px)`

❌ **No left-border accent stripes** on cards. Canvas nodes are the **lone exception** — 3px vertical stripe (`.node-rail`) indicating node type (llm violet / tool sky / logic slate / human amber / mcp violet-300).

## 10. Backgrounds

Solid surface fills. **Never** full-bleed gradient posters.

- Canvas: 24×24 dot grid (`radial-gradient(circle, ... 1px, transparent 1px) 24px 24px`, 40% opacity)
- No textures, no particle effects
- **Glassmorphism allowed in only three places:** command palette, context menu, modal overlay

## 11. Iconography

- **Lucide React** (default, 1500+ icons, shadcn-canonical, tree-shakable)
- **Phosphor React** (duotone weight) — only for empty states and splash/onboarding
- **Custom Neuron icon set** (`app/src/assets/icons/`) — only for concepts Lucide doesn't cover: `synapse`, `node-llm`, `node-tool`, `node-logic`, `node-human`, `mcp-server`, `trace-waterfall`. 24×24 grid, 1.75px stroke, round caps/joins.

Icon size scale: xs 14 / sm 16 / md 18 / lg 20 / xl 24 / 2xl 32. Default UI 16px.

❌ No emoji in product UI. ❌ No unicode-as-icon (typographic glyphs `└`, `·`, `›` are OK).

## 12. Components (canonical surfaces)

- **App shell** — 248px sidebar | main | optional 400px inspector aside
- **Sidebar** — workspace switcher, nav, sub-section, foot user card
- **Topbar** — breadcrumb, search (`⌘K` kbd), route-specific actions
- **Canvas** — dot-grid background, absolute node cards, SVG bezier edges, minimap, control strip
- **Run inspector** — header (run-id, pills), tabs (Spans / Logs / Output), span timeline, selected-span sheet
- **Marketplace card** — featured grid + plain list, install button, star + install count
- **Agent card** — avatar (NodeGlyph), name, model + temp, role, foot actions
- **Settings** — 220px nav + form pane, segmented controls, swatches
- **Terminal** — multi-pane grid (1 / 2v / 2h / 2x2 / 3x4), per-pane status border + breath, status bar
- **Tweaks panel** — floating right-bottom, glassmorphic background, accent/density/motion controls

## 13. Anti-patterns (never)

- Heavy gray-on-gray dashboard card stacks
- Full-screen AI/space gradient backgrounds
- Animated particles, Matrix rain
- Aggressive neon
- Emoji-heavy UI
- "Hacker terminal" aesthetic (mono headers everywhere)
- Gratuitous glassmorphism beyond the three approved surfaces
- Tabs-inside-tabs
- Material Design 3 pill rounding
- iOS-style segmented controls (we use our own `.seg` style)

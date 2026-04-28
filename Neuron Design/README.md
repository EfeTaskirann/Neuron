# Neuron Design System

Neuron is a desktop **Agent Development Environment (ADE)** — a multi-agent workflow tool that brings MCP server management, agent orchestration, and observability under one roof. It's positioned alongside premium-consumer dev tools like **Arc Browser** and **Things 3**, not the sterile monochrome of Linear/Vercel.

The brand metaphor is *neurons*: nodes-and-synapses, soft glow feedback, signal propagation. A workflow canvas is a network of firing neurons. Status changes "feel electric."

## Sources

This system was built from a single specification document (`design-system-spec.md` v1.0, 27 April 2026, owner: Efe). No codebase or Figma was attached. All visual decisions trace directly to that spec.

## Index

- `README.md` — this file
- `colors_and_type.css` — all design tokens (OKLCH colors, type scale, spacing, motion, radii, shadows)
- `assets/` — logo wordmark, brandmark, custom Neuron icons (synapse, node-llm, node-tool, node-logic, node-human, mcp-server, trace-waterfall)
- `preview/` — design system cards rendered in the Design System tab
- `ui_kits/desktop/` — the Neuron desktop app UI kit (canvas, sidebar, settings, run inspector, marketplace)
- `SKILL.md` — invocable skill manifest

## Content fundamentals

**Voice.** Calm, precise, and a little reverent of the craft. Reads like Things 3 ("Get things done.") meets Linear's product-spec terseness. No marketing fluff. Sentences are short. Verbs are concrete (*build, observe, orchestrate*). Avoid AI-industry buzzwords ("intelligent," "next-gen," "AI-powered").

**Casing.** Sentence case for everything in UI: titles, buttons, menu items. Title Case is reserved for proper-noun product surfaces ("MCP Marketplace," "Run Inspector"). UPPERCASE only for `overline` labels (tracking 0.08em).

**Pronouns.** Implicit "you" in CTAs ("Create workflow"), no "I" / "we" / "your AI assistant." Never anthropomorphize agents.

**Examples.**
- Title: "Where agents connect." (tagline) · TR: "Ajanların buluştuğu yer."
- Empty state: "No runs yet — start a workflow to see traces here."
- Toast: "Saved" · "Connection failed — check your API key"
- Button: "New workflow" · "Install" · "Connect server"

**Emoji.** Never in product UI. Emoji-ağırlıklı UI is an explicit anti-pattern.

**Bilingual.** Primary EN, full TR parity in the spec. Settings exposes a TR/EN toggle.

## Visual foundations

**Color.** OKLCH end-to-end (Tailwind v4 + shadcn new-york). Two brand families: **Violet** (hue 295, primary action / active state, anchor `violet-500` `#8a4cc8`) and **Midnight** (hue 258, surface and dark-mode background, base `midnight-950` `#0b1024`). Status: emerald (success), amber (human-in-the-loop / warning), rose (destructive), sky (tool-node accent), slate (logic-node neutral). **Two-tier tokens**: primitive scales (`--neuron-violet-500`) are never used directly; UI references semantic tokens (`--primary`, `--background`).

**Default mode is dark.** Dark uses `midnight-950` as background. Light uses near-white with a hint of violet (not pure white). Both modes are perceptually balanced.

**Type.** Geist Sans + Geist Mono primary, Inter as fallback. Variable fonts mandatory (animated weight transitions). Modular scale 1.200, 16px base. Display 48 / H1 32 / H2 24 / H3 20 / H4 16 / body 14 / caption 12 / overline 11. Tracking tightens with size (-0.02em → 0).

**Spacing.** 4px base grid. Named semantic spacings: `--space-gutter` 24, `--space-section` 48, `--space-stack-sm/md/lg` 8/16/32. Sidebar 64 collapsed / 260 expanded / 300 wide. Inspector 320–520 (default 400). Modal max-w-lg. Touch target minimum 40px (Things 3 hizası), critical actions 44px.

**Backgrounds.** Solid surface fills, never full-bleed gradient posters. Canvas uses a 24×24 dot grid (`radial-gradient` 1px dots at 40% opacity). No textures, no particle effects, no glassmorphism except in three places (command palette, context menu, modal overlay).

**Animation.** **Motion** library (motion/react v12+), not framer-motion. Tokens: `--ease-out` Apple-vari, `--ease-out-soft` Things-vari, `--ease-spring` Arc-vari light overshoot. Durations 120 / 200 / 300 / 400 / 600 ms. Signature patterns: hover-lift `translateY(-1px) + shadow-md` 120ms; press scale 0.98 80ms; modal enter spring 300ms; node pulse on running status (violet glow oscillation 1600ms). `prefers-reduced-motion` is respected: all spring → 150ms linear fade.

**Hover & press.** Hover = lift + soft shadow (NEVER color-only). Press = scale 0.98. Active route = `bg-accent/60` + 2px violet left bar (Arc-style).

**Borders & shadows.** All shadows are violet-tinted at very low opacity (large blur, oklch midnight-950 / 6–22%). Never sharp black. Border default `oklch(midnight-800 / 0.6)`. **Synaptic glow** tokens (`--glow-violet-sm/md`) are reserved for: hovered primary CTA, selected canvas node, running span, focus ring. Glow is the brand signature — used sparingly.

**Radii.** Default 12px (`--radius-lg`). Inputs 8, badges 4, dot indicators 2, canvas nodes & modals 16, marketplace cards 24. Never above 24 (no Material pill buttons).

**Cards.** Background `--card` (midnight-900 in dark, white in light), radius-lg or radius-xl, shadow-sm by default, shadow-md on hover with translateY(-1px). No left-border accent stripes (an anti-pattern), except for canvas nodes which use a 3px vertical stripe to indicate node type.

**Transparency & blur.** Only command palette (`backdrop-blur-xl + bg-popover/80`), context menu (`backdrop-blur-md + bg-popover/85`), modal overlay. Tauri WebView GPU budget makes broader blur expensive.

**Imagery vibe.** Cool-leaning, midnight-tinted. No warm grain, no photo-realistic AI art. Brandmark uses violet-300 → violet-700 diagonal gradient on midnight-900.

## Iconography

**Lucide React** is the default (1500+ icons, shadcn-canonical, tree-shakable). **Phosphor React** (duotone weight) is used only for empty states and the splash/onboarding "premium consumer" flourish.

**Custom Neuron icon set** in `assets/icons/`: only for concepts Lucide doesn't cover. 24×24 grid, 1.75px stroke, round caps/joins (Lucide-compatible).

- `synapse.svg` — brand mark; presynaptic dot + curved synaptic terminal + postsynaptic ring
- `node-llm.svg` — radial pulse with center dot
- `node-tool.svg` — wrench + screwdriver union
- `node-logic.svg` — if/else branching flow
- `node-human.svg` — minimal raised-hand glyph
- `mcp-server.svg` — cube with synapse terminal
- `trace-waterfall.svg` — three staggered horizontal bars

Icon size scale: xs 14 / sm 16 / md 18 / lg 20 / xl 24 / 2xl 32. Default UI 16px, button-internal 16px, sidebar collapsed 20px, canvas-node header 18px.

**No emoji. No unicode-as-icon.** A placeholder is better than a bad attempt.

## Font substitution

The spec specifies **Geist Sans + Geist Mono** (OFL, self-hosted). I have not been able to fetch the actual font files in this project, so the CSS falls back through Inter → ui-sans-serif → system-ui. **Action requested: please drop `Geist-Variable.woff2` and `GeistMono-Variable.woff2` into `fonts/`** so the brand renders correctly. Until then, expect Inter (close metric match) in previews.

## Anti-patterns (never)

- Heavy gray-on-gray dashboard card stacks
- Full-screen AI/space gradient backgrounds
- Animated particles, Matrix rain
- Aggressive neon
- Emoji-heavy UI
- "Hacker terminal" aesthetic (mono headers, etc.)
- Gratuitous glassmorphism
- Tabs-inside-tabs
- Material Design 3 pill rounding
- iOS-style segmented controls

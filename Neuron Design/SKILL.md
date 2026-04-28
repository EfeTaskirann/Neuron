---
name: neuron-design
description: Use this skill to generate well-branded interfaces and assets for Neuron, a desktop Agent Development Environment (ADE) for multi-agent workflows, MCP server management, and observability. Use for production code, prototypes, mocks, slides, or any visual artifact in the Neuron brand.
user-invocable: true
---

Read the README.md file in this skill, and explore the other available files (`colors_and_type.css`, `assets/`, `preview/`, `ui_kits/desktop/`).

## Brand at a glance

- **Metaphor**: neurons — nodes-and-synapses, soft glow, signal propagation.
- **Family**: premium-consumer dev tool. Closer to Arc Browser + Things 3 than Linear/Vercel.
- **Default mode**: dark. Background `oklch(0.135 0.032 258)` (midnight-950).
- **Primary**: violet `#8a4cc8` (`oklch(0.643 0.214 298)`, hue 295).
- **Type**: Geist Sans + Geist Mono, Inter as fallback. 16px base, modular 1.200 scale.
- **Motion library**: `motion` (motion/react), NOT framer-motion.
- **Icons**: Lucide first; Phosphor (duotone) for empty states; custom Neuron set for node types.
- **Color space**: OKLCH only. Never HSL or hex literals in UI (only in legacy SVGs).

## How to use

If creating visual artifacts (slides, mocks, throwaway prototypes):
- Copy assets out of `assets/` and import `colors_and_type.css` to inherit tokens.
- Use the UI kit components in `ui_kits/desktop/` as recipes for shells, canvases, and forms.
- Default to dark mode (`<html class="dark">`).

If working on production code:
- Tailwind v4 + shadcn/ui new-york style. All tokens live in `globals.css` `@theme inline`.
- Reference semantic tokens (`--primary`, `--background`), never primitives directly.
- Two-tier token model: primitive scales (`--neuron-violet-N`, `--neuron-midnight-N`) feed semantic vars; UI only uses semantic.

If invoked without other guidance, ask the user what they want to build, then act as an expert Neuron designer.

## Hard rules (anti-patterns — never)

- No emoji in UI. No unicode-as-icon.
- No full-bleed gradient backgrounds.
- No glassmorphism except: command palette, context menu, modal overlay.
- No left-border accent stripes on cards (canvas nodes are the lone exception).
- No Material pill buttons (radius caps at 24).
- No "hacker terminal" aesthetic.
- No HSL or hex literals in CSS.

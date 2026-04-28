---
id: ADR-0001
title: Tauri 2 over Electron for desktop shell
status: accepted
date: 2026-04-21
deciders: Efe Taşkıran
---

## Context

Neuron needs a cross-platform desktop shell. Two mainstream options:

- **Electron** — Chromium + Node.js bundled. Mature, large ecosystem, predictable rendering across platforms.
- **Tauri 2** — System WebView + Rust backend. Smaller bundle, native perf, growing ecosystem.

The app's bundle target is "premium consumer" (Things 3, Arc) — users notice 100MB+ download size, slow first launch, fan noise from Electron's bundled Chromium.

## Decision

**Tauri 2.**

## Rationale

| Concern | Tauri 2 | Electron |
|---|---|---|
| Bundle size (typical) | ~10–15 MB | ~120–180 MB |
| Idle RAM (single window) | ~80 MB | ~250 MB |
| Native perf (system WebView) | ✅ | ❌ (bundled Chromium) |
| Rust backend integration | ✅ first-class | ⚠️ via FFI |
| Cross-platform parity | ⚠️ (Edge/WebKit/Safari differ) | ✅ |
| Plugin ecosystem | smaller | mature |
| Auto-updater | built-in | tooling needed |
| Security model | smaller attack surface, capability allowlist | larger surface, IPC sandboxing optional |

For a personal-tool premium-consumer ADE: bundle size + RAM + Rust alignment outweigh ecosystem maturity. The frontend is React 18 (works in any modern WebView), so cross-WebView differences are bounded.

## Consequences

- ✅ Smaller bundle, native perf, Rust ecosystem alignment with the agent runtime
- ⚠️ Must test on three WebViews (WebView2 / WebKit / WKWebView). Compatibility issues may surface; mitigate by sticking to stable React + standard CSS (no bleeding-edge features)
- ⚠️ Smaller plugin ecosystem; some Electron-specific plugins (auto-updater for legacy code-signing) need re-implementation
- ⚠️ Tauri 2 GA'd in 2024; some breaking changes still possible. Pinned to a specific minor version.

## Revisit

If WebView compatibility becomes a chronic blocker (e.g., a mission-critical CSS feature works in 2 of 3 WebViews and feature-detect can't recover), revisit in Week 4.

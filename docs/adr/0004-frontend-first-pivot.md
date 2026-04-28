---
id: ADR-0004
title: Frontend-first pivot for Week 1
status: accepted
date: 2026-04-21
deciders: Efe Taşkıran
---

## Context

The conventional sequence for a desktop ADE is: data model → backend → API → frontend. We deliberately reversed it for Week 1.

## Decision

**Build the visual prototype first (Week 1), backend after (Week 2).** Mock data lives in `app/data.js` and `app/terminal-data.js`. UI components read from `window.NeuronData`. Backend is wired in Week 2 by replacing the mock data source — the UI stays.

## Rationale

- **Visual uncertainty was higher than data-model uncertainty.** OKLCH tokens, neuron metaphor, glow-vs-no-glow, dark-first defaults — these are decisions that benefit from rapid visual iteration. Backend schemas are derivative.
- **Mock-first captures the real shape contract.** Once the UI looks right, the data shape is defined. The backend's job in Week 2 becomes "produce this shape", not "negotiate this shape".
- **Reduces backend rework risk.** If we'd built backend first, every UI revision could trigger a schema migration. Going UI-first means schema lands once, near-final.
- **Demo-able from day one.** Week 1 already has a click-thru anyone can open in a browser — useful for design review even before the backend exists.

## Consequences

- ✅ Backend has a precise target shape from day one of Week 2
- ✅ UI was demo-ready in Week 1
- ⚠️ Frontend used CDN React + babel-standalone in Week 1 (no build step). Week 2 must migrate to Vite + TS — extra work, but mechanical
- ⚠️ Charter rule emerged: **backend mock-shapes follow frontend, never vice versa.** This rule binds WP-W2-02..08

## Anti-pattern this avoids

"Backend defines API → frontend wraps it → designer asks for visual change → backend reshape → frontend re-wraps." That loop is expensive when visual decisions are still in flux. Frontend-first inverts the dependency: visual decisions stabilize first; backend shape is the resulting export.

## Revisit

If a Week 2+ backend constraint genuinely forces a frontend reshape (e.g., a third-party MCP API has a fixed shape we cannot transform in the Rust handler), accept the reshape and document the exception. Default remains UI-first.

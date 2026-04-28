# Project Charter — Neuron

**Version:** 1.0
**Owner:** Efe Taşkıran
**Last updated:** 2026-04-28
**Status:** Active — Week 2

## Vision

Neuron is a desktop **Agent Development Environment (ADE)** for building, observing, and orchestrating multi-agent workflows. It brings MCP server management, agent runtime, terminal panes, and span-level observability under a single dark-first, premium-consumer surface.

The brand metaphor is *neurons*: nodes-and-synapses, soft glow feedback, signal propagation. Tone is closer to **Arc Browser + Things 3** than **Linear/Vercel**.

## Phases

| Phase | Window | Scope | Status |
|---|---|---|---|
| Week 1 — Design + click-thru | done | Design system + working visual prototype (CDN React + Babel) | ✅ shipped |
| Week 2 — Backend integration | active | Tauri 2 + Rust + SQLite + LangGraph; mock → real wiring | 🟢 planning |
| Week 3 — MCP + telemetry hardening | future | Real MCP install/uninstall, telemetry export, multi-workflow editor | TBD |

## Tech stack (locked)

| Layer | Choice | Rationale | ADR |
|---|---|---|---|
| Desktop shell | Tauri 2 | Bundle size, native perf, Rust alignment | 0001 |
| Backend lang | Rust 1.78+ | Type safety, FFI, system access | 0001 |
| Database | SQLite via `sqlx` | Embedded, ACID, single-file portability | — |
| Migrations | `sqlx-cli` | Versioned, reproducible, offline-mode | — |
| Frontend | React 18 + TS, Vite | Migration from CDN React; matches mock | 0004 |
| Frontend data | TanStack Query v5 | Cache + invalidate; clean mock → real swap | 0005 |
| Agent runtime | LangGraph (Python sidecar) | Mature stateful graph runtime | — |
| MCP integration | Anthropic MCP (Rust client) | Spec-canonical | — |
| Terminal sidecar | `portable-pty` (Rust) | Cross-platform PTY, no extra runtime | — |
| Tracing | Custom OTel-style spans → SQLite | Self-hosted, no external collector | — |
| Package manager | pnpm | Disk efficiency, workspaces | — |
| Secrets | OS keychain via `keyring` crate | No `.env`, no plaintext | — |

## In scope (Week 2)

- ✅ Tauri 2 scaffold at repo root (`app/` + `src-tauri/`)
- ✅ SQLite schema + migrations for agents, runs, spans, servers, workflows, nodes, edges, panes, mailbox
- ✅ Tauri command surface (`agents:list`, `runs:list`, `mcp:list`, `terminal:spawn`, …)
- ✅ LangGraph sidecar integration with one demo workflow ("Daily summary")
- ✅ MCP server install + state persistence (Filesystem, GitHub seeded)
- ✅ Terminal panes via portable-pty
- ✅ Span/trace persistence + run inspector reads from DB
- ✅ Frontend mock → real migration via TanStack Query (per ADR-0005)

## Out of scope (Week 2)

- ❌ Multi-user / cloud sync
- ❌ Auth / OAuth / passthrough credentials
- ❌ Production telemetry export (Datadog, Honeycomb, OTel collector) — Week 3
- ❌ Plugin system / third-party MCP marketplace UI
- ❌ Multi-window
- ❌ Mobile / web build targets
- ❌ Custom workflow editor UI (Week 3 — Week 2 ships one hardcoded "Daily summary")

## Hard constraints

1. **Frontend mock shape is the contract.** Backend produces data matching `Neuron Design/app/data.js` and `terminal-data.js` exactly. Backend never asks frontend to change shape.
2. **No OAuth passthrough.** API keys live in OS keychain. Never plaintext, never `.env` committed.
3. **Dark-first.** All net-new UI ships dark. Light parity desirable but never gates a release.
4. **OKLCH only for colors.** Existing legacy hex inside SVGs may stay; new CSS never introduces hex/HSL.
5. **No Drizzle / no JS ORM.** ORM lives in Rust (`sqlx`). Frontend never talks to SQLite directly.
6. **Single demo workflow.** Week 2 ships with one demonstrable workflow; multi-workflow management is iceberg.
7. **No `--no-verify` commits.** Hooks exist for a reason. If a hook fails, fix the issue.

## Roles

| Role | Holder | Authority |
|---|---|---|
| Owner | Efe Taşkıran | Final call on scope/feature/release |
| Orchestrator | Claude Code (this session) | Sub-agent delegation, verification gates, AGENT_LOG.md |
| Worker agents | Sub-agents (general-purpose) | One WP each, no cross-WP work |
| Reviewer | Claude Code (final QA pass) | Acceptance verification |

## Authority hierarchy (conflict resolution)

1. **PROJECT_CHARTER.md** (this file) — top
2. **docs/work-packages/WP-W2-XX-*.md** — WP-specific overrides
3. **design-system-spec.md** — visual decisions
4. **NEURON_TERMINAL_REPORT.md** — terminal-specific architectural decisions
5. **AGENTS.md** — agent working rules
6. **ADRs** — record rationale, never authority
7. **Existing prototype code** (`Neuron Design/`) — REFERENCE ONLY, deleted at end of Week 2

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| LangGraph Python sidecar packaging | High — desktop bundle bloat | Week 2 requires system Python 3.11+; PyOxidizer in Week 3 |
| portable-pty Windows quirks | Medium — terminal regressions | Pin crate version, test in CI matrix later |
| Frontend mock drift during migration | High — breaks UI | Strict shape parity test in WP-W2-08 acceptance |
| SQLite schema migration reverse-incompatibility | Medium | Test up + down for every migration |
| Anthropic MCP spec churn | Medium | Pin to a specific MCP version; document upgrade procedure |
| Tauri 2 minor-version breaking changes | Low | Pin to `2.x.y` exact version, manual upgrade |

## Release gate (end of Week 2)

`pnpm tauri dev` opens the app. All six routes (Workflows, Agents, Runs, Marketplace, Settings, Terminal) render real backend data. One LangGraph workflow runs end-to-end producing spans visible in the run inspector. Closing and reopening preserves state. `cargo test` and `pnpm test --run` pass.

## Cleanup at end of Week 2

`Neuron Design/` and `neuron-docs/` are deleted in WP-W2-08. Tokens, fonts, icons, and any reusable HTML/CSS migrate into `app/src/styles/` and `app/src/assets/`. The canonical layout from Week 2 forward is at repo root.

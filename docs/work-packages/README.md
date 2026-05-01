# Work Packages

Sequential delivery plan for the Neuron backend phases. Each WP is delegated to one sub-agent; orchestrator verifies before next WP.

## Week 2 — Backend integration (shipped)

### Status

| ID | Title | Status |
|---|---|---|
| WP-W2-01 | Tauri 2 scaffold | ✅ shipped |
| WP-W2-02 | SQLite schema + migrations | ✅ shipped |
| WP-W2-03 | Tauri command surface | ✅ shipped |
| WP-W2-04 | LangGraph agent runtime | ✅ shipped |
| WP-W2-05 | MCP server registry | ✅ shipped |
| WP-W2-06 | Terminal PTY sidecar | ✅ shipped |
| WP-W2-07 | Span / trace persistence | ✅ shipped |
| WP-W2-08 | Frontend mock → real wiring | ✅ shipped |

### Dependency graph

```
WP-W2-01 (scaffold)
   │
   ▼
WP-W2-02 (sqlite)
   │
   ▼
WP-W2-03 (commands) ──┬──► WP-W2-04 (agents) ──► WP-W2-07 (tracing)
                      ├──► WP-W2-05 (mcp)         │
                      └──► WP-W2-06 (terminal)    │
                                                  ▼
                                            WP-W2-08 (frontend)
                                                  ▲
                                            (also needs 04,05,06)
```

## Week 3 — MCP & telemetry hardening (active)

Detailed scope rationale, dependency narrative, and open questions
in [`WP-W3-overview.md`](./WP-W3-overview.md).

### Status

| ID | Title | Status | Blocked by | Size |
|---|---|---|---|---|
| WP-W3-01 | OS keychain + settings table | not-started | — | S |
| WP-W3-02 | MCP session pool + cancel safety | not-started | WP-W3-01 | M |
| WP-W3-03 | MCP install UX — 5 stub manifests fully wired | not-started | WP-W3-02 | M |
| WP-W3-04 | Agent runtime — cancel propagation + streaming | not-started | WP-W3-01 | M |
| WP-W3-05 | Approval UI (regex placeholder → real flow) | not-started | WP-W3-04 | M |
| WP-W3-06 | Telemetry export (OTel collector + sampling) | not-started | — | M |
| WP-W3-07 | Pane aggregates from spans | not-started | WP-W3-06 | S |
| WP-W3-08 | Multi-workflow editor + fixture system | not-started | — | L |
| WP-W3-09 | Capabilities tightening + E2E (Playwright) | not-started | 02,03,04,06,08 | M |
| WP-W3-10 | PyOxidizer Python embed | not-started | WP-W3-04 | L |

### Dependency graph

```
WP-W3-01 (keychain + settings)
   │
   ├──► WP-W3-02 (MCP pool + cancel safety) ──► WP-W3-03 (MCP install UX)
   │                                                 │
   └──► WP-W3-04 (agent cancel + streaming) ──► WP-W3-05 (approval UI)
                                                 │
                                                 ▼
WP-W3-06 (OTel + sampling) ──► WP-W3-07 (pane aggregates)
                                                 │
                                                 ▼
WP-W3-08 (workflow editor + fixtures)            │
                                                 │
                                                 ▼
                                       WP-W3-09 (capabilities + E2E)
                                                 │
WP-W3-10 (PyOxidizer) ──── parallel ─────────────┘
```

## Conventions

- File name: `WP-W2-NN-kebab-name.md`
- Frontmatter required: `id`, `title`, `owner`, `status`, `depends-on`, `acceptance-gate`
- Section order: Goal → Scope → Out of scope → Deliverables → Acceptance criteria → Verification commands → Notes / risks
- Each WP touches only its declared file scope. Cross-WP changes require a charter exception.

## Sub-agent kickoff procedure

The orchestrator (Claude Code) reads the WP file in full, spawns ONE general-purpose sub-agent with the entire WP body in the prompt, waits for completion, runs verification commands, and then asks the user before the next WP. See `AGENTS.md` for full protocol.

# Work Packages — Week 2

Sequential delivery plan for Week 2 backend integration. Each WP is delegated to one sub-agent; orchestrator verifies before next WP.

## Status

| ID | Title | Owner | Status | Blocked by |
|---|---|---|---|---|
| WP-W2-01 | Tauri 2 scaffold | TBD | not-started | — |
| WP-W2-02 | SQLite schema + migrations | TBD | not-started | WP-W2-01 |
| WP-W2-03 | Tauri command surface | TBD | not-started | WP-W2-02 |
| WP-W2-04 | LangGraph agent runtime | TBD | not-started | WP-W2-03 |
| WP-W2-05 | MCP server registry | TBD | not-started | WP-W2-03 |
| WP-W2-06 | Terminal PTY sidecar | TBD | not-started | WP-W2-03 |
| WP-W2-07 | Span / trace persistence | TBD | not-started | WP-W2-04 |
| WP-W2-08 | Frontend mock → real wiring | TBD | not-started | 03,04,05,06,07 |

## Dependency graph

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

## Conventions

- File name: `WP-W2-NN-kebab-name.md`
- Frontmatter required: `id`, `title`, `owner`, `status`, `depends-on`, `acceptance-gate`
- Section order: Goal → Scope → Out of scope → Deliverables → Acceptance criteria → Verification commands → Notes / risks
- Each WP touches only its declared file scope. Cross-WP changes require a charter exception.

## Sub-agent kickoff procedure

The orchestrator (Claude Code) reads the WP file in full, spawns ONE general-purpose sub-agent with the entire WP body in the prompt, waits for completion, runs verification commands, and then asks the user before the next WP. See `AGENTS.md` for full protocol.

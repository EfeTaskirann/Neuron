# AGENTS.md

Working rules for agents (Claude Code, Cursor, etc.) on this repo.

## Authority

Read `PROJECT_CHARTER.md` first. It is the top of the conflict-resolution chain. If a charter rule contradicts these working rules, charter wins.

Hierarchy: Charter → WP file → design-system-spec → NEURON_TERMINAL_REPORT → AGENTS.md → ADRs → existing code.

## Path conventions (Week 2 forward)

| Where | What lives there |
|---|---|
| Repo root | `app/`, `src-tauri/`, `docs/`, top-level docs (Charter, AGENTS, design-spec, terminal-report), `pnpm-workspace.yaml`, `package.json`, `Cargo.toml` (workspace) |
| `app/` | React 18 + TS frontend (Vite) |
| `app/src/` | Source (components, hooks, routes, styles) |
| `app/src/hooks/` | TanStack Query hooks (one per top-level NeuronData key) |
| `app/src/lib/bindings.ts` | Auto-generated Tauri command types (specta) — do not edit by hand |
| `src-tauri/` | Rust backend |
| `src-tauri/src/` | Rust source |
| `src-tauri/src/commands/` | Tauri command handlers (one file per namespace) |
| `src-tauri/src/sidecar/` | Sidecar process supervisors (agent runtime, PTY) |
| `src-tauri/migrations/` | sqlx migrations (`NNNN_name.sql`) |
| `docs/adr/` | Architecture Decision Records (`NNNN-kebab-name.md`) |
| `docs/specs/` | Specifications (`feature-name.md`) |
| `docs/work-packages/` | Work packages (`WP-W2-NN-kebab-name.md`) |

`Neuron Design/` and `neuron-docs/` are reference-only. Do NOT edit. They are deleted in WP-W2-08.

## Sub-agent delegation

Each work package is delegated to ONE sub-agent. The orchestrator never does multi-WP work in a single sub-agent call.

Pattern:
1. Orchestrator reads WP-W2-XX.md fully (no summary substitute)
2. Spawns sub-agent (`subagent_type=general-purpose`) with the full WP body in prompt
3. Sub-agent does the WP, returns a summary
4. Orchestrator verifies acceptance criteria and runs verification commands itself
5. Orchestrator updates `AGENT_LOG.md`
6. Orchestrator asks user before next WP

## Sub-agent prompt template

When spawning, include:

1. The full text of WP-W2-XX (paste verbatim)
2. Authority pointer: "Read `PROJECT_CHARTER.md` if uncertain about scope"
3. Acceptance criteria as a checklist the sub-agent must self-verify
4. Verification commands the sub-agent must run before returning
5. Exact list of files it may modify (no creep beyond WP scope)
6. Reminders:
   - Do NOT change frontend mock shape (`Neuron Design/app/data.js`, `terminal-data.js`)
   - Do NOT add a build step beyond what the WP authorizes
   - Do NOT introduce technologies outside Charter's tech-stack table

## Commits

- Conventional commits: `feat:`, `fix:`, `chore:`, `docs:`, `refactor:`, `test:`
- Subject ≤ 70 chars
- Body in present tense, "why" not "what"
- Co-authored line at the end:
  `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`
- One WP = one commit (or one PR with focused commits). No multi-WP commits.

## Verification gates (pre-commit)

Each WP must pass:
- `pnpm typecheck`
- `pnpm test --run`
- `cargo check --manifest-path src-tauri/Cargo.toml`
- `cargo test --manifest-path src-tauri/Cargo.toml`
- WP-specific verification (listed in WP file under "Verification commands")

If a gate fails, the WP is NOT done. Fix or escalate to user. **Never bypass with `--no-verify`.**

## AGENT_LOG.md (running journal)

After each WP, append at top of `AGENT_LOG.md` (create if missing):

```markdown
## [ISO timestamp] WP-W2-XX completed
- sub-agent: general-purpose
- files changed: [count + key paths]
- acceptance: ✅ pass / ❌ [detail]
- commit SHA: [sha]
- next: WP-W2-YY
```

## Hard rules

- Never assume. If a doc is missing, halt and ask the user.
- Never skip verification. Trust the gates, not your impression.
- Never edit `Neuron Design/` or `neuron-docs/`. They are reference only.
- Never put secrets in committed files. `.env*`, `*.pem` are gitignored.
- Never `git push --force` to `main`. Branch + PR for risky changes.
- Never proceed to the next WP without user confirmation.
- Never amend a commit that has been pushed.

## Reporting tone

Factual. No "great!", no "perfect!". Format:

```
✅/❌ WP-W2-XX [title]
- files changed: N
- acceptance: pass/fail
- commit: SHA
- next: WP-W2-YY
- continue?
```

On failure:

```
❌ WP-W2-XX failed at acceptance step [N]
- error: [full message, no truncation]
- likely cause: [1 sentence]
- suggestion: [rollback / retry / ask user]
- HALT.
```

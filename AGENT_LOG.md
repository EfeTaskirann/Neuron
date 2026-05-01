# Agent Log

Running journal of agent-driven changes. Newest entry on top. See `AGENTS.md` § "AGENT_LOG.md" for format.

---

## 2026-05-02T00:45Z WP-W3-01 completed (Week 3 kickoff)

- sub-agent: general-purpose
- files changed: 12 (4 new, 8 modified)
  - new: `src-tauri/src/secrets.rs`, `src-tauri/src/commands/secrets.rs`, `src-tauri/src/commands/settings.rs`, `src-tauri/migrations/0004_settings.sql`
  - modified: `src-tauri/Cargo.toml` + `Cargo.lock` (`keyring = "=3.6.3"` per-target deps), `src-tauri/src/lib.rs` (mod + 7 commands), `src-tauri/src/commands/{mod.rs, me.rs}`, `src-tauri/src/db.rs` (test rename + count bump 11→12, migration count 3→4), `src-tauri/src/mcp/registry.rs` (`resolve_env` → `crate::secrets::get_secret`), `app/src/lib/bindings.ts` (regen +28)
- commit SHAs:
  - `621b02c` `chore(lint): wire react-hooks plugin and fix surfaced warnings` — pre-W3-01 lint gate fix (52575ca's eslint-disable directives referenced an unloaded plugin; this commit also fixes two genuine warnings the rule then surfaced in `Canvas.tsx` and `Terminal.tsx` cleanup ref capture)
  - `a351cd2` `feat: WP-W3-01 keychain (Rust) + settings table` — the WP itself
- acceptance: ✅ pass — orchestrator independently re-ran every gate after sub-agent return
  - `cargo check` → exit 0
  - `cargo test --lib` → exit 0, **135 passed, 0 failed, 4 ignored** (110 prior + 25 new = +25)
  - `pnpm gen:bindings` → 7 new commands (`secretsSet/Has/Delete`, `settingsGet/Set/Delete/List`); `secretsGet` deliberately absent
  - `pnpm typecheck`, `pnpm test --run` (17/17), `pnpm lint` → all exit 0 (lint pass requires `621b02c` first)
- key implementation choices
  - **`secrets:get` is NOT a command**: per WP-W3-01 §3 design decision, secret values never cross the IPC boundary. Only `secrets:has` (boolean presence) is exposed. Consumers (`mcp:install`, `runs:create`) read directly via `crate::secrets::get_secret`.
  - **Service name parity with Python**: Rust `keyring::Entry::new("neuron", key)` matches `agent_runtime/secrets.py:SERVICE = "neuron"`. One `secrets:set('anthropic', ...)` write is readable by both Rust MCP runtime and Python agent runtime.
  - **`keyring` per-target deps**: 3.x requires opt-in to a backend feature. Three `[target.'cfg(...)'.dependencies]` blocks (Windows / macOS / Linux) so each platform pulls only its native backend. Pinned to `=3.6.3`.
  - **Generic API**: per the 2026-05-01 owner decision, no Rust enum or const list of provider names. The `crate::secrets::*` API is generic over `key: &str` so future providers (`gemini`/`groq`/`together`) land as Settings-UI dropdown changes, not API changes.
  - **Settings table is `WITHOUT ROWID`** — small key/value table; saves a btree level on lookup.
  - **Dot-namespaced keys**: `user.name`, `workspace.name`, future `otel.endpoint`, `theme.mode`. The namespace becomes a fixed enum once W3-09 narrows capabilities; for now the column is plain TEXT.
- bindings regenerated: yes (+28 lines, 7 new commands)
- branch: `main` (local; not pushed; 2 new commits on top of `a8866de`)
- known caveats / followups
  - Tauri capability for `secrets:*` and `settings:*` rides on tauri-specta's invoke handler; no `capabilities/default.json` change in this WP. Explicit allowlist enumeration is W3-09.
  - `settings:list` returns specta-tuple wire shape `[string, string][]`. If the W3-09 Settings UI prefers `{key, value}[]`, that's a one-line model refactor.
  - W3-06 (telemetry export, parallel-authored in `a8866de`) is unblocked and ready for sub-agent dispatch.
- next: WP-W3-06 (telemetry export — OTLP/JSON sweep + insert-time sampling)

---

## 2026-04-30T18:32:54Z WP-W2-08 prep + 4-agent followup completed
- sub-agents: B (mcp catalog), C (me:get), A (panes domain), D (operasyonel hygiene) — dispatched in 4 parallel terminals per `tasks/agent-briefs-2026-04-29.md`
- commits: `7596386` (pre-package), `52b270f` (4-agent package), `e1a813c` (bindings regen)
- new files (across the 3 commits):
  - sub-agent additions: `src-tauri/src/tuning.rs`, `src-tauri/src/commands/util.rs`, `src-tauri/src/commands/me.rs`, `src-tauri/migrations/0003_panes_approval.sql`, 6 MCP manifests (`linear/notion/stripe/sentry/figma/memory.json`), `tasks/agent-briefs-2026-04-29.md`
  - pre-package additions (bug-fix + refactor + contract amendments): `docs/adr/0007-id-strategy.md`, `docs/adr/0008-sidecar-ipc-framing.md`, `src-tauri/migrations/0002_constraints.sql`, `src-tauri/src/events.rs`, `src-tauri/src/time.rs`, `tasks/refactor-v1.md`, `tasks/report-29-04-26.md`, `tasks/todo.md`
- modifications: `PROJECT_CHARTER.md` (+Constraints #1 carve-out, #8 timestamp, #9 id), `docs/adr/0006-…md` (`.` → `:` separator amendment), `models.rs` (Mailbox `from`/`to` rename per Charter #1, Pane 5 new fields, `ApprovalBanner` + `Me`/`User`/`Workspace` types), `Neuron Design/app/data.js` (s1-s12 → slug realign), `lib.rs` (`mod tuning`/`util`, subscriber init, `commands::me::me_get` registration), `db.rs` / `sidecar/{agent,terminal}.rs` / `mcp/client.rs` (`eprintln!` → `tracing::*`, constants → `crate::tuning::*`), `commands/runs.rs` (rollback inline → `commands::util::finalise_run_with`), `commands/terminal.rs` (Pane SELECT genişle + status-guarded approval blob parse), `commands/mailbox.rs` (validation messages aligned to wire `from`/`to`), `Cargo.toml` (+`tracing`, +`tracing-subscriber`), regen `app/src/lib/bindings.ts`
- new commands: `me:get`
- mcp catalog: 6 → 12 servers (Linear, Notion, Stripe, Sentry, Figma, Memory added as catalog-only stubs)
- tracing adopted, all active `eprintln!` (test/bin scope hariç) migrated
- acceptance: ✅ pass — orchestrator independently re-ran the gates after every sub-agent return + after each commit
  - `cargo test --lib` → exit 0, **102 passed, 3 ignored** (95 prior + 2 me + 3 panes + 2 util)
  - `cargo check --tests` → exit 0 (4 unrelated `unused_mut` warnings on `mcp/client.rs:570/572`)
  - `cargo run --bin export-bindings` → bindings.ts regenerated (+120/-13)
  - `pnpm typecheck` → exit 0
  - `pnpm test --run` → 1 file 2 tests passed
  - `pnpm lint --max-warnings=0` → exit 0
- key implementation choices (this round)
  - **Charter Constraint #1 carve-out**: display-derived strings (`started: "2 min ago"`, `uptime: "12m 04s"`) ship as raw `_at`/`_ms` fields; frontend hook layer derives the human form. Single bounded carve-out — structural fields remain non-negotiable.
  - **MailboxEntry wire revert**: `fromPane`/`toPane` → `from`/`to` with `#[serde(rename)]`; Rust fields keep `_pane` for SQL column binding. ADR-0006 separator promoted from `.` to `:` to match Tauri 2.10 reality.
  - **ApprovalBanner persistence**: `panes.last_approval_json TEXT` (migration 0003); reader-side regex extraction with placeholder fallback; `terminal_list` parses **only when** `status = 'awaiting_approval'`.
  - **MCP catalog stub pattern**: 6 new catalog-only manifests (`spawn: null`); `mcp:install` against them surfaces `McpServerSpawnFailed` cleanly. `installed: true|false` mock flag mismatch deferred to Week 3 G2.
  - **`tracing` over `eprintln!`**: setup hook initialises `tracing_subscriber::fmt().with_env_filter(…).try_init()` (panic-safe for tests). `RUST_LOG=neuron=debug` honored.
  - **File-level staging**: pre-package and 4-agent diffs were physically interleaved in modified source files (models.rs, lib.rs, db.rs, sidecar/*, mcp/*, commands/{mod,runs,terminal}.rs). Atomic 5-commit split would have required hunk-level staging; A2-modified 3-commit split shipped instead. Commit messages disclose the constraint.
- bindings regenerated: yes (`Pane` 5 fields, `ApprovalBanner`, `Me`/`User`/`Workspace`, `commands.meGet`)
- branch: `main` (local; not pushed; **3 new commits on top of `7dba715`**)
- next: WP-W2-07 (span/trace persistence — completes WP-04 event chain; depends only on WP-04) or WP-W2-08 (frontend mock→real wiring — biggest WP, 7 routes + cleanup; now unblocked since pre-package + 4-agent closed all known wire-shape gaps)

---

## 2026-04-29T12:50:37Z WP-W2-06 completed
- sub-agent: general-purpose
- files changed: 8 in commit `351c234`
  - new: `src-tauri/src/sidecar/terminal.rs` (TerminalRegistry, ring buffer, regex status detection, CSI stripping, agent-kind inference)
  - modified: `src-tauri/src/commands/terminal.rs` (replaced WP-W2-03 stubs; added `terminalWrite`, `terminalResize`, `terminalLines`), `src-tauri/src/lib.rs` (registry wiring + `RunEvent::ExitRequested` shutdown hook), `src-tauri/src/models.rs` (`PaneSpawnInput` confirmed, `PaneLine` added), `src-tauri/src/sidecar/mod.rs` (`pub mod terminal`), `src-tauri/Cargo.toml` (+`portable-pty`, +`regex`), `Cargo.lock`, `app/src/lib/bindings.ts` (regenerated)
- commit SHA: `351c234`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **86 passed, 3 ignored** (75 prior + 11 new terminal tests; 2 prior + 1 new opt-in shell-spawn integration)
  - new tests verify: ring buffer overflow drops oldest 1,000, CSI stripper preserves text + removes cursor controls, awaiting-approval regex matches Claude/Codex/Gemini canonical prompts, agent-kind inference from cmd, default shell resolution per platform, registry concurrency (no shared mutable state across panes), kill-pane is idempotent for already-dead children, ring-buffer flush on close populates `pane_lines`, since_seq cursor reads from DB after pane close, resize zero-dim rejection, unknown-pane 404
  - `cargo check` → exit 0
  - `cargo run --bin export-bindings` → bindings.ts regenerated with `terminalWrite`, `terminalResize`, `terminalLines` typed wrappers
  - frontend regression: `pnpm typecheck/lint/test --run` all green (1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / migrations / db.rs / mcp / sidecar/agent.rs / other-command files touched
- key implementation choices
  - **Event name**: `panes:{id}:line` payload `{ k, text, seq }` (`:` separator per ADR-0006 carryover; matches WP-04's `runs:{id}:span` and WP-05's `mcp:installed/uninstalled`).
  - **Reader runtime**: `tokio::task::spawn_blocking` because `portable-pty` exposes `std::io::Read` (sync). CRLF normalised to LF for storage; CSI sequences stripped before persisting to `pane_lines`; raw text preserved in live event payload for xterm.js rendering in WP-W2-08.
  - **Master+writer drop on child exit**: required for Windows ConPTY (the reader pipe is a clone independent of the master Arc). Without dropping, the blocking `read()` never unblocks.
  - **Default shell resolution** (Windows): `pwsh.exe` if `where.exe pwsh.exe` succeeds, else `powershell.exe`. Resolved at spawn time, not cached.
  - **Agent-kind inference** from cmd substring: `claude-code`, `codex`, `gemini`, default `shell`. Persisted in `panes.agent_kind`.
  - **Ring buffer**: 5,000 lines per pane in memory; on overflow drop oldest 1,000; on child exit OR `kill_pane`, flush remaining ring lines to `pane_lines` table for hydration after restart.
  - **Status state machine**: `idle → starting → running → (awaiting_approval ↔ running) → success | error`; awaiting transitions driven by per-agent regex set on the last 5 lines.
  - **Idempotent kill**: tolerates Win32 `ERROR_INVALID_PARAMETER (87)` and Unix `ESRCH` so killing a child that exited mid-flight returns Ok.
- bindings regenerated: yes (3 new typed wrappers + `PaneLine` struct)
- branch: `main` (local; not pushed; **20 commits ahead of `origin/main`**)
- next: WP-W2-07 (span/trace persistence — completes the WP-04 event chain) or WP-W2-08 (frontend mock→real wiring — biggest WP, 7 routes + cleanup)

---

## 2026-04-29T11:36:15Z WP-W2-05 completed
- sub-agent: general-purpose
- files changed: 17 in commit `1ffa084`
  - new module: `src-tauri/src/mcp/{mod,client,registry,manifests}.rs`
  - new manifests: `src-tauri/src/mcp/manifests/{filesystem,github,postgres,browser,slack,vector-db}.json` (6 servers)
  - new doc: `src-tauri/MCP.md` (spec version pin `2024-11-05` + `npx` runtime requirement)
  - modified: `src-tauri/src/commands/mcp.rs` (replaced WP-W2-03 stubs; added `mcpListTools`, `mcpCallTool`), `src-tauri/src/db.rs` (added `seed_mcp_servers`), `src-tauri/src/{error,lib,models}.rs`, `app/src/lib/bindings.ts` (regenerated)
- commit SHA: `1ffa084`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **75 passed, 2 ignored** (56 prior + 19 new MCP tests; 1 prior `#[ignore]`d + 1 new `integration_filesystem_install_and_call` opt-in)
  - new tests verify: NDJSON frame round-trip, registry CRUD, seed idempotency, persist-across-pool-reopen, list ordering (featured first), uninstall flow, install + tools/list integration against real `@modelcontextprotocol/server-filesystem`
  - `cargo check` → exit 0
  - 19 unit tests + 1 ignored npx integration test pass
  - `cargo run --bin export-bindings` → bindings.ts regenerated with `mcpListTools`, `mcpCallTool`, `Tool`, `ToolContent`, `CallToolResult` typed wrappers
  - frontend regression: `pnpm typecheck/lint/test --run` all green (1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / migrations / sidecar / other-command files touched
- key implementation choices
  - **Wire format**: NDJSON over stdio (one UTF-8 JSON object per line, `\n`-terminated) per MCP spec — different from WP-W2-04's length-prefixed framing.
  - **`argsJson: string`** on `mcpCallTool` IPC (not `serde_json::Value`): specta produces broken TS for arbitrary JSON values, so callers `JSON.stringify(args)`. Pragma documented in `commands/mcp.rs`.
  - **No migration file**: seed is data-dependent on `manifests/*.json`, so `db::seed_mcp_servers` runs from `db::init` after migrations (parallels WP-W2-04's `seed_demo_workflow`). Idempotent via `INSERT OR IGNORE`; user-toggled `installed` flag never overwritten on re-seed.
  - **Filesystem server fully wired**: install → spawn `npx -y @modelcontextprotocol/server-filesystem <path>` → `tools/list` → persist `server_tools` rows. Other 5 seeded servers (github, postgres, browser, slack, vector-db) surface `mcp_server_spawn_failed` if the user tries to install them — Week 3 wires the full pipeline. The `mcp:list` returns all 6 with metadata regardless.
  - **No session pool**: each `mcp:callTool` re-spawns the server. Pooling deferred to Week 3 alongside sandbox isolation.
  - **MCP spec version pinned** to `2024-11-05` in MCP.md (Charter risk register's "spec churn" mitigation).
  - **Event names**: `mcp:installed` / `mcp:uninstalled` (`:` separator per ADR-0006 carryover; matches WP-W2-03's mailbox precedent).
- bindings regenerated: yes (new typed wrappers for the 2 new commands + 3 new types)
- branch: `main` (local; not pushed; **17 commits ahead of `origin/main`**)
- next: WP-W2-06 (terminal sidecar) or WP-W2-07 (tracing — depends on WP-W2-04, also unblocked)

---

## 2026-04-28T23:33:29Z WP-W2-04 completed
- sub-agent: general-purpose
- files changed: 23 in commit `5d390e4`
  - new: `src-tauri/sidecar/agent_runtime/` (Python project: pyproject.toml, uv.lock, .python-version, README, .gitignore, `agent_runtime/{__init__,__main__,framing,secrets}.py`, `agent_runtime/workflows/{__init__,daily_summary}.py`, `agent_runtime/tests/{test_framing,test_daily_summary}.py`)
  - new: `src-tauri/src/sidecar/{mod.rs, agent.rs, framing.rs}`
  - modified: `Cargo.lock`, `src-tauri/Cargo.toml` (tokio +process,+io-util features), `src-tauri/src/{lib.rs, commands/runs.rs, error.rs}`, `app/src/lib/bindings.ts` (regenerated, 9-line diff in `runsCreate` docstring; signature unchanged)
- commit SHA: `5d390e4`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test` → exit 0, **56 passing, 1 ignored** (47 prior + 9 new sidecar tests; the ignored test is the live-Python integration `integration_spawn_then_shutdown_kills_child`, opt-in)
  - python tests (sub-agent ran via `uv run pytest` in sidecar dir): 13 passing (7 framing round-trip + 6 daily_summary including `no_api_key` path)
  - `cargo check` → exit 0
  - `runs:create` now dispatches to sidecar when `SidecarHandle` is in `app.try_state`; happy-path test asserts run row with `status='running'` and zero spans
  - `RunEvent::ExitRequested` hook calls `SidecarHandle::shutdown()`; `kill_on_drop(true)` is the seatbelt
  - no_api_key path emits structured span `attrs.error='no_api_key'`, run ends with `status='error'` (asserted by `test_no_api_key_path_emits_error_span_and_ends_in_error`)
  - frontend regression: `pnpm typecheck/lint/test --run` all green (still 1 file 2 tests)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / design-spec / terminal-report / migrations files touched
- key implementation choices
  - **Event naming**: emits `runs:{id}:span` with a `kind: "created"|"updated"|"closed"` discriminator (NOT three event names). Stays consistent with the WP-W2-03 `:` substitution forced by Tauri 2.10's `IllegalEventName` panic on `.`.
  - **Stdio framing**: 4-byte big-endian u32 length + UTF-8 JSON body, 16 MiB cap, symmetric on both sides. Codec round-trip-tested on Python and Rust independently.
  - **LangGraph pin**: `>=0.2,<0.3` per WP §"Notes / risks".
  - **Python pin**: `.python-version → 3.11` (uv installed Python 3.11.15 in `.venv`); host's 3.14 left out because LangGraph 0.2.x compatibility on 3.14 is unproven.
  - **API keys**: `keyring.get_password('neuron', 'anthropic')` per Charter §"Hard constraints" #2; never logged.
  - **Span emission**: explicit from each LangGraph node, NOT via LangChain ChatModel callbacks (per WP §"Sub-agent reminders").
  - **Mock tool nodes**: `fetch_docs`/`search_web` return canned strings; real MCP tools land in WP-W2-05.
- bindings regenerated: yes (9-line diff, docstring-only on `runsCreate`)
- branch: `main` (local; not pushed; **13 commits ahead of origin/main**)
- next: WP-W2-05 (MCP registry), WP-W2-06 (terminal sidecar), or WP-W2-07 (tracing — depends on WP-W2-04). Three options, all unblocked by this WP.

---

## 2026-04-28T22:40:30Z WP-W2-03 completed
- sub-agent: general-purpose (initial pass rate-limited mid-execution; orchestrator-led fix-up pass landed on a fresh general-purpose sub-agent invocation)
- files changed: 22 in commit `35c4a85`
  - new: `src-tauri/src/{models.rs, error.rs, test_support.rs, bin/export-bindings.rs}`, `src-tauri/src/commands/{agents,workflows,runs,mcp,terminal,mailbox}.rs`, `src-tauri/test-manifest.{rc,xml}`, `app/src/lib/bindings.ts` (302 lines, generated)
  - modified: `Cargo.lock`, `pnpm-lock.yaml`, `src-tauri/Cargo.toml`, `src-tauri/build.rs`, `src-tauri/src/lib.rs`, `src-tauri/src/commands/{mod.rs, health.rs}`, `app/package.json`, `app/eslint.config.js`
- commit SHA: `35c4a85`
- acceptance: ✅ pass — orchestrator independently re-ran all gates after sub-agent return
  - `cargo check` → exit 0
  - `cargo test --manifest-path src-tauri/Cargo.toml` → exit 0, **47/47 tests passing** (5 db + 39 command + 3 error tests)
  - 17 commands compiled and registered: agents (5: list/get/create/update/delete), workflows (2: list/get), runs (4: list/get/create/cancel), mcp (3: list/install/uninstall), terminal (3: list/spawn/kill), mailbox (2: list/emit) — plus existing `health_db` smoke
  - `app/src/lib/bindings.ts` generated by `cargo run --bin export-bindings`; tauri-specta provides typed JS wrappers (`commands.agentsList()`)
  - `pnpm typecheck` → exit 0 (after adding `@tauri-apps/api ^2.10.1` to `app/package.json`)
  - `pnpm lint` → exit 0 (`app/src/lib/bindings.ts` added to `app/eslint.config.js` ignores; tauri-specta emits one unavoidable `any` cast)
  - `mailbox:new` event fires after `mailbox:emit` succeeds (verified by `mailbox::tests::mailbox_emit_fires_mailbox_new_event`)
  - AppError shape `{ kind, message }` verified by per-namespace error-path tests (e.g. `agents_get_unknown_id_is_not_found`, `runs_cancel_already_done_is_conflict`)
  - Stub commands return only documented side effects (`runs:create` inserts `status='running'` row with no spans; `mcp:install` flips `installed=1`; `terminal:spawn` inserts `status='idle'` pane row)
  - frontend regression: `pnpm test --run` → 1 file 2 tests still passing
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `docs/` / Charter / AGENTS.md / design-spec / terminal-report files touched
- deviations from WP-W2-03 strict file allowlist (orchestrator-authorized):
  - `app/package.json`: +`@tauri-apps/api ^2.10.1` (required for `bindings.ts` to import `__TAURI_INVOKE`; without it `pnpm typecheck` fails)
  - `app/eslint.config.js`: `src/lib/bindings.ts` added to `ignores` (generated file, single unavoidable `any`)
  - `src-tauri/src/bin/export-bindings.rs`: orchestrator pre-applied `CARGO_MANIFEST_DIR` path anchor to fix relative-path bug that wrote `bindings.ts` to `Desktop/app/...` outside the workspace
  - `src-tauri/build.rs` modified + `src-tauri/test-manifest.{rc,xml}` added: Common-Controls v6 application manifest required for cargo lib-test exes on Windows. `tauri-runtime-wry` imports `TaskDialogIndirect` from comctl32 v6; without a manifest the test binary fails at startup with `STATUS_ENTRYPOINT_NOT_FOUND` (0xC0000139). Fix: disable `tauri-build`'s default manifest, compile own via `rc.exe` in `build.rs`, emit unscoped `cargo:rustc-link-arg=` so production + test exes share one manifest section
- **⚠️ ADR-0006 divergence — needs follow-up**: ADR-0006 specifies event names of shape `{domain}.{id?}.{verb}` with `.` as separator (e.g. `mailbox.new`, `runs.{id}.span`). Tauri 2.10's event-name validator rejects `.` and panics with `IllegalEventName`. Code uses `:` substitution: `mailbox:new`, `agents:changed`, `mcp:installed`, `mcp:uninstalled`. Future WP-W2-06 (`panes:{id}:line`) and WP-W2-07 (`runs:{id}:span`) will follow the same `:` pattern. The shape `{domain}{sep}{id?}{sep}{verb}` is preserved with `:` instead of `.`. **ADR-0006 should be amended in a small follow-up commit** to either (a) record the `.` → `:` substitution, or (b) document that `.` works (if a future Tauri version relaxes the validator).
- IPC naming reality: Tauri's `#[command]` macro forbids `:` in Rust identifiers; the IPC wire uses underscore form (`agents_list`). The colon-namespace ergonomics specified by Charter live in tauri-specta's typed JS wrappers (`commands.agentsList()` etc.) consumed via `import { commands } from './lib/bindings'` in WP-W2-08.
- WP-W2-02 carryover resolved: `health_db` is registered alongside the 17 new commands; tauri-specta exposes it as `commands.healthDb()` on the JS side.
- `.bridgespace/` directory (user's IDE hook artifact) is untracked and intentionally excluded from this commit. Add to `.gitignore` in a separate small commit if desired.
- branch: `main` (local; not pushed; 9 commits ahead of `origin/main`)
- next: WP-W2-04 (LangGraph agent runtime), WP-W2-05 (MCP registry), or WP-W2-06 (terminal sidecar) — all three depend only on WP-W2-03

---

## 2026-04-28T19:27:40Z WP-W2-02 completed
- sub-agent: general-purpose
- files changed: 8 (`src-tauri/Cargo.toml`, `src-tauri/migrations/0001_init.sql`, `src-tauri/src/db.rs` (new module, 244 lines incl. 5 tests), `src-tauri/src/lib.rs` (setup hook + manage pool + register health_db), `src-tauri/src/commands/mod.rs` (new), `src-tauri/src/commands/health.rs` (new, smoke command), `src-tauri/.sqlx/query-976b52de…json` (offline cache), `Cargo.lock`)
- commit SHA: `8870de6`
- acceptance: ✅ pass — orchestrator independently re-ran the gates after sub-agent return
  - `cargo test --manifest-path src-tauri/Cargo.toml -- db` → exit 0, **5/5 tests passing**:
    - `migration_creates_all_eleven_tables` — list matches expected sorted set
    - `pragma_foreign_keys_is_on_per_connection` — verified across 3 connections
    - `migrations_are_idempotent` — second-launch + fresh-pool, exactly 1 row in `_sqlx_migrations`
    - `pool_can_insert_and_select` — round-trip via the agents table
    - `macro_query_uses_offline_cache` — `sqlx::query_scalar!` compiles + runs against `.sqlx/`
  - `cargo check` → exit 0, 0.70s warm
  - 11 schema tables present in `0001_init.sql`: agents, edges, mailbox, nodes, pane_lines, panes, runs, runs_spans, server_tools, servers, workflows
  - `.sqlx/` offline cache committed (1 query JSON for the test macro)
  - DbPool wired via `app.manage(pool)` in `lib.rs` setup hook; smoke command `health_db` returns `{ tables, foreignKeysOn }`
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` / `neuron-docs/` / `app/` / `docs/` files touched
  - frontend regression check: `pnpm typecheck` ✅, `pnpm lint` ✅, `pnpm test --run` ✅ (still 1 file 2 tests — Hello Neuron + OKLCH)
  - manual `pnpm tauri dev` + `sqlite3 .tables` verification: pending — sandbox cannot launch desktop window
- naming deviation (transparent): smoke command exposed as `health_db` (underscore) instead of charter-canonical `health:db` (colon). Reason: Tauri 2.x's `#[tauri::command]` does not ship a stable `rename = "..."` attribute without extra crates; per WP-W2-02 explicit allowance the underscore form is acceptable for this WP only. WP-W2-03 introduces `tauri-specta` binding generation which will alias the IPC surface back to colon-namespaced names.
- informational: actual Tauri bundle identifier is `app.neuron.desktop` (set in WP-W2-01's `tauri.conf.json`) — DB file lands at `%APPDATA%\app.neuron.desktop\neuron.db` on Windows, NOT the WP body's example `com.neuron.dev`. WP body comment was illustrative; behaviour follows the actual identifier.
- toolchain: `sqlx-cli` installed via `cargo install sqlx-cli --no-default-features --features sqlite` (one-time, on user PATH; not a project dependency)
- branch: `main` (local; not pushed)
- next: WP-W2-03 (Tauri command surface) — depends on WP-W2-02 only

---

## 2026-04-28T18:26:30Z WP-W2-01 completed
- sub-agent: general-purpose
- files changed: 19 (key: `app/{package.json,vite.config.ts,vitest.config.ts,index.html,tsconfig*.json,eslint.config.js}`, `app/src/{main.tsx,App.tsx,App.test.tsx,styles.css,test/setup.ts,vite-env.d.ts}`, `src-tauri/{Cargo.toml,build.rs,tauri.conf.json,src/{main.rs,lib.rs},capabilities/default.json,icons/}`, root `{package.json,pnpm-workspace.yaml,Cargo.toml,Cargo.lock,pnpm-lock.yaml,.nvmrc,.gitignore,.cargo/config.toml}`)
- commit SHA: `d0bbffa`
- acceptance: ✅ pass — orchestrator independently re-ran all 4 non-interactive gates after sub-agent return
  - `pnpm typecheck` → exit 0 (`tsc -b --noEmit`)
  - `pnpm lint` → exit 0 (`eslint --max-warnings=0`)
  - `pnpm test --run` → exit 0 (1 file, 2 tests: "Hello Neuron" render + `--background` OKLCH token assertion)
  - `cargo check --manifest-path src-tauri/Cargo.toml` → exit 0 (0.60s on warm cache)
  - prototype isolation: `git diff HEAD~1 HEAD` shows zero `Neuron Design/` or `neuron-docs/` files touched
  - manual `pnpm tauri dev` window-open verification: pending — sandbox cannot open desktop window; user must verify
- deviation from sub-agent file allowlist: `.cargo/config.toml` added (out-of-allowlist). Reason: this Windows host has a partial MSVC + KitsRoot10 registry mismatch causing `cargo check` to fail with `LNK1181: oldnames.lib / legacy_stdio_definitions.lib` despite both libs existing in alternate directories. The config.toml adds project-local `/LIBPATH` rustflags using 8.3 short paths so cargo can compile Tauri's Win32 dependency tree end-to-end. Sub-agent disclosed transparently in its report; orchestrator accepts the deviation as project-local, Charter-compatible (no new tech, no global state mutation), and necessary to reach the WP's `cargo check exits 0` acceptance gate on this host.
- toolchain bootstrap performed by sub-agent: `pnpm@10.33.2` via `npm i -g`, Rust `1.95.0 stable` via `rustup-init` (minimal profile). Both placed `cargo`/`pnpm` on user PATH.
- branch: `main` (local; not pushed)
- next: WP-W2-02 (SQLite schema + migrations) — depends on this WP only

---

## 2026-04-28T17:30:54Z docs/review-2026-04-28 completed
- sub-agent: orchestrator-direct (manual route per SUBAGENT-PROMPT § "Notes for the orchestrator" — docs-only pass, sub-agent delegation overhead skipped)
- files changed: 4 (1 added: `docs/adr/0006-event-naming-and-mailbox-realtime.md`; 3 modified: `docs/work-packages/WP-W2-01-tauri-scaffold.md`, `docs/work-packages/WP-W2-03-command-surface.md`, `docs/work-packages/WP-W2-08-frontend-wiring.md`)
- commits (in order): `8d61b75`, `9b24047`, `8024b5d`
- acceptance: ✅ pass — 3 commits in correct order, 4 files diff against `main`, working tree clean, all `Co-Authored-By` trailers present, no files outside `docs/` touched
- branch: `docs/review-2026-04-28` (local; not pushed)
- next: orchestrator awaits user confirmation to merge `docs/review-2026-04-28` → `main` and proceed to WP-W2-01 delegation

---
id: WP-W3-11
title: Swarm runtime foundation — Claude Code subprocess substrate
owner: TBD
status: not-started
depends-on: [WP-W3-01]
acceptance-gate: "`swarm:profiles_list` reads `.md` agent profiles from disk; `swarm:test_invoke` spawns a real `claude` CLI subprocess with the profile's persona, sends one user message, and returns the parsed `result` event. Subscription auth is preserved (no `ANTHROPIC_API_KEY` injected)."
---

## Goal

Stand up the substrate the architectural report (`report/Neuron
Multi-Agent Orchestration — Mimari Analiz Raporu`, §13.4 "Hafta 1")
prescribes for Phase 1: a transport layer that spawns `claude` CLI
subprocesses with `--input-format stream-json --output-format
stream-json`, a `.md`-based agent profile loader, and one smoke
command (`swarm:test_invoke`) that proves the end-to-end pipe.

Nothing else. Coordinator state machine, persistent chat, retry
loop, broadcast / fan-out, multi-pane UI, MCP per-agent config,
profile permission-mode enforcement — every higher-layer concern
belongs to a later WP (W3-12 onward). This WP is substrate only.

The smoke surface is what unblocks W3-12: once we can spawn a real
specialist and read its `result` event back over stream-json, the
state machine has somewhere concrete to write to.

## Why now / scope justification

Per the architectural report's §13.3 smoke validations (`Say
exactly: 'A done'.`, parallel 3-spawn, OAuth-path verification),
the user has already verified the `claude` CLI substrate works
ad-hoc from `~/AppData/Local/Temp`. This WP is the codified
version of those smoke tests — the same calls, but inside Neuron's
Tauri command surface, profile-driven, with proper env-cleanup so
the subscription channel (Pro/Max) is preserved over the
`ANTHROPIC_API_KEY` path.

**Charter alignment.** This WP introduces a new tech in the agent
runtime layer — `claude` CLI subprocess pool — alongside the
existing LangGraph Python sidecar. The two coexist:

- **LangGraph sidecar** continues to power the scripted "Daily
  summary" demo workflow (Charter Phases row, Week 2 release gate).
- **Swarm runtime** (this WP and successors) is the new
  user-facing multi-agent orchestration feature: user picks an
  agent team, talks to a Coordinator, the Coordinator dispatches
  specialists.

Charter §"Tech stack (locked)" gains a row **in the same commit
as this WP** (per owner directive 2026-05-05): the WP and its
Charter amendment are atomic so future readers tracing the
swarm-runtime origin land on a single SHA.

## Scope

### 1. New module `src-tauri/src/swarm/`

Sibling to `src-tauri/src/sidecar/` (which hosts the LangGraph
agent supervisor and the PTY terminal registry). Layout:

```
src-tauri/src/swarm/
├── mod.rs          // pub mod binding; pub mod profile; pub mod transport;
├── binding.rs      // claude CLI invocation helpers (path resolve, env clean, args)
├── profile.rs      // .md frontmatter parser + Profile struct + ProfileRegistry
└── transport.rs    // SubprocessTransport: spawn → stream-json → result event
```

Wired into `lib.rs` with `pub mod swarm;` next to `pub mod sidecar;`.

### 2. `swarm::profile` — `.md` profile loader

Profile dirs (resolution order, first match wins per `id`):

1. **User-edited (per-install)**: `<app_data_dir>/agents/*.md`
   (Tauri's `path::app_data_dir`, same root the SQLite DB lives
   under). Per owner directive 2026-05-05: choosing
   `app_data_dir` (not `~/.neuron/agents`) so a clean reinstall
   wipes user-edited profiles together with the rest of the
   install state — no orphan `~/.neuron` survives uninstall.
   W3-12+ may add a workspace-folder override.
2. **Bundled defaults**: `src-tauri/src/swarm/agents/*.md`
   embedded via `include_dir!` so the app ships with three
   working profiles out of the box.

Phase 1 does NOT pre-populate `<app_data_dir>/agents/` — the
directory is read if it exists, ignored if absent. Bundled
profiles are always available. A user who wants to override
`scout.md` first creates the dir manually and drops a file with
the same `id`.

Profile file format (frontmatter + body, mirrors the
architectural report's §4.1 example):

```markdown
---
id: scout
version: 1.0.0
role: Scout
description: Read-only repo investigator
allowed_tools: ["Read", "Grep", "Glob"]
permission_mode: plan
max_turns: 8
---
# Scout

Sen bir read-only repo araştırmacısısın...
```

**Phase 1 frontmatter contract** (only these fields parsed; extras
in the file are tolerated but unused — they belong to W3-12+):

```rust
pub struct Profile {
    pub id: String,                     // required, [a-z][a-z0-9-]{1,40}
    pub version: String,                // required, semver
    pub role: String,                   // required, free text
    pub description: String,            // required
    pub allowed_tools: Vec<String>,     // optional, default ["Read"]
    pub permission_mode: PermissionMode,// optional, default Plan
    pub max_turns: u32,                 // optional, default 8
    pub body: String,                   // everything after the closing `---`
    pub source_path: PathBuf,           // for diagnostics; not serialized
}
```

`PermissionMode`:

```rust
pub enum PermissionMode { Plan, AcceptEdits, AcceptAll }
```

(`Plan` ≈ `--permission-mode plan` in the CLI; W3-12 enforces.)

Frontmatter parser is hand-rolled (no `gray_matter` dep — the
parser is ~50 LOC and avoids a transitive `pest`/`yaml-rust`
chain). Body is everything after the second `---` line, trimmed.

`ProfileRegistry`:

```rust
pub struct ProfileRegistry { /* HashMap<String, Profile> */ }

impl ProfileRegistry {
    pub fn load_from(dirs: &[PathBuf]) -> Result<Self, AppError>;
    pub fn get(&self, id: &str) -> Option<&Profile>;
    pub fn list(&self) -> Vec<&Profile>;
}
```

**Validation rules** (errors are `AppError::InvalidInput`):

- `id` must be non-empty and match `^[a-z][a-z0-9-]{1,40}$`.
- `version` must parse as a 3-part semver-ish pattern (`d+.d+.d+`).
- `allowed_tools` must be a JSON-style array of strings (parsed by
  `serde_json` from the YAML-ish line value — accept both
  `["Read","Edit"]` and `[Read, Edit]` styles by normalizing
  before parse).
- Two profiles with the same `id` from different dirs → workspace
  wins, bundled becomes a `tracing::debug!` line; not an error.

### 3. `swarm::binding` — `claude` CLI invocation helpers

```rust
/// Result of resolving the claude binary on this host.
pub struct ClaudeBinary { pub path: PathBuf }

pub fn resolve_claude_binary() -> Result<ClaudeBinary, AppError>;

/// Build the env map for a `claude` spawn that MUST use the
/// subscription OAuth (Pro/Max) channel, NOT an API key.
/// Strips ANTHROPIC_API_KEY, USE_BEDROCK, USE_VERTEX, USE_FOUNDRY.
pub fn subscription_env() -> HashMap<String, String>;

/// Build the argv for a one-shot per-invoke specialist call.
/// Returns the args as a `Vec<String>` so callers can log them and
/// `tokio::process::Command::args` consumes them.
pub fn build_specialist_args(
    profile: &Profile,
    system_prompt_file: &Path,  // already-written tmp file
) -> Vec<String>;
```

Resolution order in `resolve_claude_binary()` (mirrors
`sidecar::agent::resolve_python` style):

1. `NEURON_CLAUDE_BIN` env var (test/dev override).
2. `which::which("claude")` — covers macOS Homebrew, Linux package
   managers, Windows where `claude.cmd` is on PATH after the
   official installer.
3. Platform-specific common locations:
   - Windows: `%LOCALAPPDATA%\Programs\claude\claude.cmd`.
   - macOS: `~/.npm-global/bin/claude`.
   - Linux: `~/.local/bin/claude`.
4. Else: `AppError::ClaudeBinaryMissing` with a CTA pointing at
   `https://docs.claude.com/en/docs/claude-code/setup`.

`build_specialist_args()` produces, in order:

```
-p
--input-format stream-json
--output-format stream-json
--verbose
--append-system-prompt-file <system_prompt_file>
--max-turns <profile.max_turns>
--dangerously-skip-permissions   // gated below; see "Permissions"
--allowedTools "<comma-joined profile.allowed_tools>"
```

**Permissions note (Phase 1 only)**: `--dangerously-skip-permissions`
is set **iff** `permission_mode != Plan`. For `Plan` we pass
`--permission-mode plan` instead. W3-12 introduces a richer mapping
(per-tool allow / deny lists from the profile). This WP keeps the
gate binary so the smoke command can run without a UI prompt.

### 4. `swarm::transport` — `SubprocessTransport`

```rust
pub struct SubprocessTransport;

#[derive(Debug, Serialize, Deserialize)]
pub struct InvokeResult {
    pub session_id: String,
    pub assistant_text: String,
    pub total_cost_usd: f64,
    pub turn_count: u32,
}

impl SubprocessTransport {
    pub async fn invoke(
        profile: &Profile,
        user_message: &str,
        timeout: Duration,
    ) -> Result<InvokeResult, AppError>;
}
```

Implementation outline (~120 LOC):

1. Resolve `claude` binary; bail if missing.
2. Write `profile.body` to a tmp file under `app_data_dir/swarm/tmp/<uuid>.md`
   so `--append-system-prompt-file` has a path. Persona is the
   body verbatim — no template substitution.
3. Build `subscription_env()` map.
4. `tokio::process::Command::new(claude_path)`
   `.envs(env)` `.args(args)` `.stdin(Stdio::piped())`
   `.stdout(Stdio::piped())` `.stderr(Stdio::piped())`
   `.kill_on_drop(true)` `.spawn()`.
5. Write one NDJSON user message to stdin:
   `{"type":"user","message":{"role":"user","content":"<user_message>"}}\n`,
   then close stdin.
6. Drain stdout line-by-line via `BufReader::lines()`. Each line
   is one JSON event. Track:
   - `system.init` → capture `session_id`.
   - `assistant.message` → append text deltas.
   - `result.success` → capture `total_cost_usd`, `turn_count`,
     final assistant text. **Stop reading.**
   - `result.error` → bail with `AppError::SwarmInvoke(reason)`.
7. Drain stderr in a parallel task into a ring buffer (capped 64 KiB);
   surface the last segment in error messages.
8. `tokio::time::timeout` wraps the whole read loop. On timeout
   → child is killed (kill_on_drop covers it on `drop`), bail
   `AppError::Timeout`.
9. Wait for child to exit; if exit code != 0 and we don't have a
   `result.success`, surface stderr tail.

Tmp file is deleted on the happy path; left in place on error so
the caller can grep it.

### 5. Tauri commands `swarm:profiles_list` and `swarm:test_invoke`

New file `src-tauri/src/commands/swarm.rs`:

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_profiles_list(
    app: AppHandle<R>,
) -> Result<Vec<ProfileSummary>, AppError>;

#[tauri::command]
#[specta::specta]
pub async fn swarm_test_invoke<R: Runtime>(
    app: AppHandle<R>,
    profile_id: String,
    user_message: String,
) -> Result<InvokeResult, AppError>;
```

`ProfileSummary` is a wire-friendly subset of `Profile` (no `body`,
no `source_path`). `InvokeResult` is the same shape returned by
`SubprocessTransport::invoke`, exposed on the IPC surface.

`swarm:test_invoke` is a smoke / debug command — it runs once,
returns once, no streaming. W3-12 introduces the streaming variant
that emits per-event Tauri events for the UI.

Registered in `lib.rs::specta_builder_for_export` under a
`// swarm` comment block.

### 6. Bundled default profiles (3)

Three profiles ship in the binary so a future Coordinator
(W3-12+) can exercise a *minimal three-stage flow*
(`scout → planner → builder`) end-to-end without writing custom
profiles. Even before W3-12, the user can manually drive the
pipeline by chaining three `swarm:test_invoke` calls — the
substrate is proven against more than one persona, not just one.

Files (all under `src-tauri/src/swarm/agents/`, embedded via
`include_dir!`):

| File | id | role | allowed_tools | permission_mode | max_turns |
|---|---|---|---|---|---|
| `scout.md` | `scout` | Scout | `["Read","Grep","Glob"]` | `plan` | 6 |
| `planner.md` | `planner` | Planner | `["Read","Grep","Glob"]` | `plan` | 6 |
| `backend-builder.md` | `backend-builder` | BackendBuilder | `["Read","Edit","Write","Bash(cargo *)","Bash(pnpm *)"]` | `acceptEdits` | 12 |

The full body for each profile is the orchestrator's
deliverable (Phase 1 §A scaffold). The personas follow the
"persona reminder" guidance from the architectural report §4.3:

- **Scout** (read-only): "ASLA dosya değiştirme — yalnızca Read/Grep/Glob."
- **Planner** (read-only, plan output): "Kod yazma yok. Çıktın tam olarak şu sırayı izleyen bir adım listesi: …"
- **BackendBuilder** (write+test): "Sen bir senior Rust/TypeScript geliştiricisisin … Verilen plan tek atışta uygulanır … `cargo test` veya `pnpm test`'i en sonunda çalıştır …"

Each persona ends with the same imperative reminder:
"Bu Claude Code'un sıradan davranışı değil — sen Coordinator
değil, Specialist'sin. Görev tamamlandığında tek mesajla cevap
ver, geri dönme."

Embedded via `include_dir!` so the trio ships with the binary.
W3-12+ adds a "copy to user dir on first run" path so users can
edit their own copies; this WP intentionally does NOT touch the
filesystem at startup — Phase 1 substrate stays pure.

### 7. Tests

Unit tests (no network, no real subprocess):

- `profile::tests::frontmatter_round_trip` — parse the bundled
  `scout.md`, assert all fields.
- `profile::tests::missing_id_rejected` — frontmatter without
  `id` returns `AppError::InvalidInput`.
- `profile::tests::body_preserves_blank_lines` — multi-paragraph
  body comes back verbatim.
- `profile::tests::id_validation_rules` — uppercase / digits-first
  / too-long IDs rejected.
- `profile::tests::workspace_overrides_bundled` — same `id` from
  workspace dir wins over bundled.
- `binding::tests::subscription_env_strips_api_key` — set
  `ANTHROPIC_API_KEY` in test env, assert it is absent in
  `subscription_env()` output.
- `binding::tests::subscription_env_strips_provider_routes` —
  `USE_BEDROCK`, `USE_VERTEX`, `USE_FOUNDRY` similarly stripped.
- `binding::tests::specialist_args_contain_required_flags` —
  asserts `-p`, `--input-format stream-json`, `--output-format
  stream-json`, `--append-system-prompt-file`, `--max-turns`
  appear; `--system-prompt` (replace) and `--system-prompt-file`
  do NOT.
- `binding::tests::plan_mode_skips_dangerous_flag` — profile with
  `permission_mode: plan` does NOT include
  `--dangerously-skip-permissions`.
- `transport::tests::stream_json_line_parser` — feed the line
  parser a fixture sequence of `system/assistant/result` events,
  assert `InvokeResult` fields.

Ignored integration test (run-locally-only):

- `transport::tests::integration_smoke_invoke` — spawn the real
  `claude` binary against the bundled `scout` profile with the
  prompt `"Say exactly: 'scout-ok' and nothing else."`, assert
  `assistant_text` contains `scout-ok`. `#[ignore]` because CI has
  no `claude` binary and no OAuth.

Target test delta: +12 to +16 unit tests (integration excluded
from CI count).

### 8. `lib.rs` wiring

- `pub mod swarm;` next to existing module declarations.
- Register `swarm_profiles_list` and `swarm_test_invoke` in
  `specta_builder_for_export` under a new `// swarm` comment
  block.
- No setup-hook wiring needed yet (no long-running task; profile
  loading is lazy on first command call).

### 9. Bindings

```bash
pnpm gen:bindings
```

Expect `bindings.ts` to gain:

- `commands.swarmProfilesList` → `Promise<ProfileSummary[]>`
- `commands.swarmTestInvoke(profileId, userMessage)` →
  `Promise<InvokeResult>`
- New types: `ProfileSummary`, `InvokeResult`, `PermissionMode`.

Verify with `pnpm gen:bindings:check`.

## Out of scope

- ❌ Coordinator state machine / "şef" agent (W3-12)
- ❌ Persistent Coordinator chat session / `--resume` handling
- ❌ Multi-pane UI surface for swarm specialists (W3-14)
- ❌ Verdict JSON schema + robust JSON extraction (W3-13)
- ❌ Retry loop with feedback / `MAX_RETRIES` (W3-13)
- ❌ Broadcast / fan-out (parallel Builder ∥ Reviewer) (W3-13)
- ❌ Per-agent MCP config (`--mcp-config`) (W3-13)
- ❌ SQLite persistence of swarm jobs / transcripts (W3-12 — uses
  the existing migrations cadence)
- ❌ Streaming partial deltas to the UI (W3-12 — needs an event
  channel; this WP is one-shot only)
- ❌ Cost / token budget meter (W3-14)
- ❌ Profile editor UI (W3-14)
- ❌ Marketplace / agent share (post-W3)
- ❌ BYOK API key transport (post-W3 — `AnthropicAPITransport`
  lands once subscription path is proven)

## Acceptance criteria

- [ ] `src-tauri/src/swarm/{mod,binding,profile,transport}.rs`
      exist; module declared in `lib.rs`
- [ ] `src-tauri/src/swarm/agents/{scout,planner,backend-builder}.md`
      exist and are embedded via `include_dir!`
- [ ] `swarm:profiles_list` returns exactly 3 entries on a fresh
      install (the bundled `scout`, `planner`, `backend-builder`)
- [ ] `swarm:test_invoke('scout', '<msg>')` IPC compiles, types
      end-to-end, and the integration test (`#[ignore]`)
      successfully spawns `claude` and reads back a `result` event
      when run locally with `cargo test -- --ignored`
- [ ] `subscription_env()` strips `ANTHROPIC_API_KEY`,
      `USE_BEDROCK`, `USE_VERTEX`, `USE_FOUNDRY`
- [ ] No `--system-prompt` / `--system-prompt-file` (replace mode)
      anywhere; only `--append-system-prompt-file`
- [ ] No `eprintln!` introduced; all diagnostic output via
      `tracing::*`
- [ ] No new `unsafe` block
- [ ] All Week-2 + Week-3-prior tests still pass (regression: 153
      + new tests)
- [ ] `bindings.ts` regenerated; `pnpm gen:bindings:check` passes
- [ ] All 3 sample profiles (`scout.md`, `planner.md`,
      `backend-builder.md`) ship in the binary
- [ ] **Charter amended in the SAME commit** as the WP (per
      owner directive 2026-05-05): `PROJECT_CHARTER.md`
      §"Tech stack (locked)" gains a row reading
      `Swarm runtime | claude CLI subprocess pool | local-only
      multi-agent orchestration; subscription OAuth; coexists
      with LangGraph sidecar | (no ADR yet)`
- [ ] **`WP-W3-overview.md` Status table** gains the row
      `WP-W3-11 | Swarm runtime foundation | TBD | not-started |
      WP-W3-01 | M` — also in the same commit

## Verification commands

```bash
# Rust gates
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Bindings sync
pnpm gen:bindings
pnpm gen:bindings:check

# Frontend gates
pnpm typecheck
pnpm test --run
pnpm lint

# Optional: real-claude integration smoke (manual; needs
# `claude login` already done and Pro/Max subscription)
cargo test --manifest-path src-tauri/Cargo.toml \
    -- swarm::transport::tests::integration_smoke_invoke --ignored \
    --nocapture
```

## Notes / risks

- **Subscription auth fragility.** Pro/Max OAuth refresh inside
  the spawned subprocess can occasionally prompt for re-login on
  device-code flow. The transport surfaces the stderr tail in the
  error message so the user gets a clear "run `claude login`"
  signal. W3-12 may add a Tauri-level banner for this state.
- **Stream-json buffer overflow.** Default OS pipe buffers can
  deadlock if a `claude` response (megabyte-class assistant
  message) fills before our reader drains. Phase 1 mitigation:
  the stdout reader is a dedicated `tokio::spawn` that reads to
  EOF / `result` event; never blocks on parent-side queue. Stress
  test: feed `Count slowly to 100` (the smoke prompt the user
  already validated) — should succeed without timeout at
  `60s`-default budget.
- **Windows quirks.** Anti-virus scans of `claude.cmd` can add
  3–8s to first-spawn cold start. The integration test sets a
  `60s` timeout to absorb this. Document the warm-cache speed in
  the AGENT_LOG entry once measured.
- **Cold start cost.** Every per-invoke specialist call pays the
  full `claude` CLI boot (config read, MCP init). Phase 1 accepts
  this; W3-13 may pool sessions for hot specialists.
- **`include_dir!` cost.** Embedding `scout.md` adds ≈1 KiB to
  the binary. Acceptable.
- **Charter divergence note.** Adding a second agent runtime
  (alongside LangGraph) is a tech-stack expansion. Charter §"Tech
  stack (locked)" must record the addition. The WP body lists
  this as a separate orchestrator commit so the WP itself stays
  pure-code; the orchestrator authors the Charter amendment in
  the same commit window.
- **No DB tables yet.** Phase 1 substrate is stateless. State
  (job rows, transcripts, retry history) lands in W3-12 once the
  state machine has somewhere to write. Migrations cadence (next
  is `0006_swarm_jobs.sql`) is reserved for W3-12 to claim.
- **Why no Python bridge.** The Python sidecar's `agent_runtime`
  is **not** the right layer to host this — it's LangGraph-shaped
  and serves the scripted demo workflow. Adding `claude`
  subprocess management on the Rust side keeps the two runtimes
  cleanly separate; W3-12 onward never imports anything from
  `agent_runtime/`.
- **One-shot, not streaming.** `swarm:test_invoke` returns once
  the `result` event arrives. UI streaming (per-event Tauri
  emits) is W3-12. The architectural report's §3.3 hybrid
  ("persistent Coordinator + per-invoke Specialists") fits this
  cleanly — the per-invoke side is what we ship now.

## Sub-agent reminders

- Read `report/Neuron Multi-Agent Orchestration` §3 (subprocess
  pattern) and §13 (smoke validations) before touching any code.
  The smoke prompts in §13.3 are the basis of the integration
  test fixture.
- Read `src-tauri/src/sidecar/agent.rs` for the `Command` /
  `Child` / `BufReader` / `kill_on_drop` patterns. Mirror them in
  `swarm/transport.rs` — one supervisor per call instead of one
  long-running supervisor.
- Read `src-tauri/src/secrets.rs` to confirm: this WP does NOT
  read API keys. Subscription is OAuth-only; the env strip is the
  only auth-related code.
- DO NOT add a new dep unless absolutely required. Hand-roll the
  YAML frontmatter parse; do not pull `gray_matter` /
  `serde_yaml`. `include_dir = "0.7"` is the one new dep
  authorized.
- DO NOT spawn the LangGraph sidecar from this module. The two
  runtimes are independent; cross-imports are forbidden.
- DO NOT introduce any new `unsafe`, `panic!`, or `unwrap()` in
  hot paths. Use `?` and `AppError::*` everywhere.
- Per `AGENTS.md`: one WP = one commit. Do not split frontmatter
  parse / transport / commands into separate commits.

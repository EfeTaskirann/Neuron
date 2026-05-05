# Swarm Runtime — Phase 1 (WP-W3-11) execution plan

Source: `docs/work-packages/WP-W3-11-swarm-foundation.md`.
Architectural ground truth: `report/Neuron Multi-Agent
Orchestration — Mimari Analiz Raporu` §3 (subprocess pattern), §4
(profile system), §13 (smoke validations).

This file is the orchestrator's tracking checklist; it lives until
WP-W3-11 lands and then becomes a "Review" appendix per the
existing `tasks/todo.md` precedent.

## Status

- [x] Phase 0 (substrate smoke validations) — done ad-hoc by user
      from `~/AppData/Local/Temp` on 2026-05-04. Prompts used:
      `Say exactly: 'A done'.` / `Say exactly: 'B done'.` /
      `Say exactly: 'C done'.` (parallel-3), `Count slowly to 100`
      (stream-json round-trip), `Bash tool ile 'echo hello'
      çalıştır.` (tool whitelist negative), `Say 'auth-ok'.`
      (OAuth path), `Say 'restart-ok'.` (kill+restart),
      `Bana 'merhaba' de.` (Turkish baseline).
- [x] Owner approval (2026-05-05): WP-W3-11 scope confirmed,
      hybrid dispatch (orchestrator scaffold + Charter,
      sub-agent parser/transport/tests), 3 bundled profiles,
      `app_data_dir/agents/` profile dir, Charter amendment in
      the same commit as the WP.

## Plan checklist

### A. Planning + Charter (orchestrator, folded into the WP commit)

Per owner directive 2026-05-05, A and B–J land in **one commit**
with the message `feat: WP-W3-11 swarm runtime foundation`.

- [x] Update `WP-W3-11-swarm-foundation.md` to reflect the 3
      resolved questions (3 profiles / app_data_dir / same commit)
- [x] Update this file (`tasks/swarm-phase-1.md`) — resolutions
      marked, dispatch decision recorded
- [ ] Add row to `PROJECT_CHARTER.md` §"Tech stack (locked)":
      `Swarm runtime | claude CLI subprocess pool | local-only
      multi-agent orchestration; subscription OAuth; coexists
      with LangGraph sidecar | (no ADR yet)`
- [ ] Append entry to `docs/work-packages/WP-W3-overview.md`
      Status table (`WP-W3-11 | Swarm runtime foundation | TBD |
      not-started | WP-W3-01 | M`)
- [ ] Append "Cross-runtime decisions" addendum to
      `WP-W3-overview.md` §"Owner decisions (resolved)" — record
      LangGraph-coexist + W3-04-deferred decisions so per-WP
      authors don't re-litigate

### B. Module scaffold

- [x] Create `src-tauri/src/swarm/{mod,binding,profile,transport}.rs`
      (sub-agent)
- [x] Create `src-tauri/src/swarm/agents/scout.md` (orchestrator)
- [x] Create `src-tauri/src/swarm/agents/planner.md` (orchestrator)
- [x] Create `src-tauri/src/swarm/agents/backend-builder.md` (orchestrator)
- [x] Wire `pub mod swarm;` into `src-tauri/src/lib.rs` (sub-agent)
- [x] Add `include_dir = "=0.7.4"` to `src-tauri/Cargo.toml`
      (orchestrator)
- [x] Add `which = "=4.4.2"` to `src-tauri/Cargo.toml` for
      `claude` binary resolution (orchestrator)

### C–F. Sub-agent deliverables (all marked done)

All sub-agent tasks completed and orchestrator-verified —
detailed checklist replaced with the post-flight summary in §K
(Review) below to keep this file scannable. Sub-agent's full
report (deviations, dep-pin status, integration test compile
status) preserved verbatim in the conversation transcript.

### G. Tests

- [x] All 11 unit tests + 1 `#[ignore]`d integration test
      landed (sub-agent overshot the +12 floor with +18 new
      unit tests; details in §K).

### H. Bindings + offline cache

- [x] `pnpm gen:bindings` regenerated `app/src/lib/bindings.ts`
      with `swarmProfilesList`, `swarmTestInvoke`,
      `ProfileSummary`, `InvokeResult`, `PermissionMode`.
- [ ] `pnpm gen:bindings:check` will exit 0 only AFTER the WP
      commit lands (currently exit 1 because `git diff
      --exit-code` reports the staged-but-uncommitted
      regeneration; this is the documented pre-commit state per
      WP-W3-01 / WP-W3-06 precedent).

### I. Verification gates (orchestrator-rerun, post-sub-agent)

- [x] `cargo check --manifest-path src-tauri/Cargo.toml` → exit 0
- [x] `cargo test --manifest-path src-tauri/Cargo.toml --lib` →
      exit 0, **181 passed; 0 failed; 5 ignored**
- [x] `pnpm typecheck` → exit 0
- [x] `pnpm test --run` → exit 0, **17/17 frontend tests**
- [x] `pnpm lint` → exit 0
- [x] `pnpm gen:bindings:check` → exit 1 (expected pre-commit;
      diff is exactly the 5 new exports + 1 new union type)
- [ ] Manual integration smoke (owner-driven, post-commit):
      `cargo test -- integration_smoke_invoke --ignored
      --nocapture` against a logged-in `claude` CLI; the
      bundled `scout` profile should round-trip a `Say exactly:
      'scout-ok'.` prompt within 60s

### J. Commit (pending owner approval)

- [ ] One WP commit (per `AGENTS.md`): `feat: WP-W3-11 swarm
      runtime foundation (claude subprocess + profile loader)`
- [ ] AGENT_LOG entry per the existing template

### K. Review

#### Outcome

WP-W3-11 implemented end-to-end. Substrate is live: `.md`
profile parse, `claude` CLI spawn helpers, stream-json
subprocess transport, two Tauri commands (`swarm:profiles_list`,
`swarm:test_invoke`), 3 bundled profiles
(`scout`/`planner`/`backend-builder`). All Charter / WP / docs
amendments folded into a single planned commit per owner
directive.

#### Files created (sub-agent)

- `src-tauri/src/swarm/mod.rs`
- `src-tauri/src/swarm/profile.rs`
- `src-tauri/src/swarm/binding.rs`
- `src-tauri/src/swarm/transport.rs`
- `src-tauri/src/commands/swarm.rs`

#### Files modified (sub-agent + orchestrator)

- `src-tauri/src/error.rs` — `+ClaudeBinaryMissing`,
  `+SwarmInvoke`, `+Timeout` (sub-agent)
- `src-tauri/src/models.rs` — `+ProfileSummary` IPC type
  (sub-agent)
- `src-tauri/src/commands/mod.rs` — `+pub mod swarm;` (sub-agent)
- `src-tauri/src/lib.rs` — `+pub mod swarm;` + 2 commands
  registered under `// swarm` (sub-agent)
- `src-tauri/Cargo.toml` — `+include_dir = "=0.7.4"`,
  `+which = "=4.4.2"` (orchestrator)
- `app/src/lib/bindings.ts` — regenerated, +5 exports
  (sub-agent ran `pnpm gen:bindings`)
- `PROJECT_CHARTER.md` — `+Swarm runtime` row in tech stack
  table (orchestrator)
- `docs/work-packages/WP-W3-overview.md` — `+W3-11` status row,
  `+Owner decision #4` block, dep-graph updated (orchestrator)
- `docs/work-packages/WP-W3-11-swarm-foundation.md` — created
  earlier this session, refined with resolutions (orchestrator)
- `tasks/swarm-phase-1.md` — this file (orchestrator)

#### Files created (orchestrator, scaffold)

- `src-tauri/src/swarm/agents/scout.md`
- `src-tauri/src/swarm/agents/planner.md`
- `src-tauri/src/swarm/agents/backend-builder.md`

#### Test delta

163 → 181 (+18 unit tests). Sub-agent overshot the WP §7 floor
(+12) because (a) `PermissionMode` parser dual-form support is
load-bearing for workspace authoring, (b) `allowed_tools` JSON
parse needed both quoted+unquoted variants tested, (c) ring
buffer truncation logic warranted its own assertion.

#### Sub-agent deviations (all owner/orchestrator-acceptable)

1. **`ProfileRegistry::load_from` signature**:
   `Option<&Path>` (workspace dir only) instead of WP's
   `&[PathBuf]`. The dispatch prompt explicitly authorized
   this; bundled walk is hardcoded inside the registry. Cleaner.
2. **`PermissionMode` parser permissive**: accepts both
   `accept-edits` (kebab) and `acceptEdits` (camel). Bundled
   `backend-builder.md` ships camel; WP text used kebab. Both
   parse successfully — removes a foot-gun for workspace
   authors. Unit-tested.
3. **Defensive `.env_remove()` calls** in `transport.rs` on
   the `Command` builder: belt-and-suspenders alongside the
   `subscription_env()` map, because `Command::envs()` merges
   rather than replaces. Hardening, not a deviation from intent.
4. **No dep-version bumps required**: `=0.7.4` and `=4.4.2`
   resolved via existing Cargo.lock; no orchestrator follow-up
   needed.

#### Known follow-ups (W3-12 candidates)

- `swarm:test_invoke` is one-shot, returns once `result` event
  arrives. UI streaming (per-event Tauri emits) is W3-12.
- No DB tables; W3-12 introduces `swarm_jobs`,
  `swarm_messages`, `swarm_profiles_seen` for state machine
  persistence (next migration is `0006_swarm_jobs.sql`).
- Coordinator FSM, retry loop, broadcast / fan-out (parallel
  Builder ∥ Reviewer), Verdict schema + robust JSON parser,
  multi-pane UI surface — all explicitly W3-12+ per WP "Out of
  scope".
- W3-04 (LangGraph cancel + streaming) deferred indefinitely
  per Owner decision #4; re-evaluate at W3-08 close.

## Dispatch decision (resolved 2026-05-05)

✅ **Hybrid dispatch** confirmed by owner.

**Orchestrator scope (this session)**:
- WP-W3-11 final form (resolutions baked in)
- `tasks/swarm-phase-1.md` execution log (this file)
- `PROJECT_CHARTER.md` §"Tech stack (locked)" amendment
- `docs/work-packages/WP-W3-overview.md` Status table append
- `src-tauri/src/swarm/agents/{scout,planner,backend-builder}.md`
  — the 3 bundled profiles (data, not Rust code)
- `src-tauri/Cargo.toml` dep additions (`include_dir`, `which`)
  with exact pins + comments — orchestrator owns dep choices

**Sub-agent scope (general-purpose, dispatched after scaffold)**:
- All `.rs` code under `src-tauri/src/swarm/`
- `src-tauri/src/commands/swarm.rs`
- `src-tauri/src/lib.rs` wiring (`pub mod swarm;` + command
  registration)
- All tests (unit + ignored integration)
- `pnpm gen:bindings` regen
- Verification gates self-run (cargo check / cargo test --lib /
  pnpm typecheck / pnpm test --run / pnpm lint /
  pnpm gen:bindings:check)

**Orchestrator post-dispatch**:
- Independent re-run of every gate (per AGENTS.md verification
  protocol — never trust sub-agent's self-report)
- Single commit per AGENTS.md "one WP = one commit"
- AGENT_LOG entry

## Resolved questions (2026-05-05)

1. **Bundled profile set for Phase 1.** ✅ **3 profiles**
   (`scout` + `planner` + `backend-builder`). Owner reasoning:
   "Faz 1 sonunda gerçek bir mini-workflow test edebilmem için
   (scout araştırır, planner plan yapar, builder kod yazar — gate
   logic yok ama akış var)." Implication: even before the W3-12
   Coordinator FSM lands, the user can manually chain three
   `swarm:test_invoke` calls to drive a full
   investigate→plan→build sequence; the substrate is exercised
   against more than one persona.
2. **Workspace profile dir location.** ✅ `app_data_dir/agents/`
   (NOT `~/.neuron/agents`). Owner reasoning: "Kullanıcı yeniden
   install ederse profil de kaybolsun (clean state)." Implication:
   no orphan dotfile survives uninstall; the swarm filesystem
   footprint is fully encapsulated by the same dir that hosts
   `neuron.db`.
3. **Charter amendment timing.** ✅ **Same commit as the WP**.
   Owner reasoning: "WP'nin Charter'a referans verdiği commit
   history'de kopuk kalmasın." Implication: the WP-W3-11 commit
   touches `PROJECT_CHARTER.md` AND `docs/work-packages/WP-W3-overview.md`
   AND the new code in one atomic landing. Diverges from the
   WP-W3-overview "separate planning commit" cadence used by
   W3-01 / W3-06 — recorded here so the discipline change is
   visible.

## Cross-runtime decisions (2026-05-05)

The owner asked: *"LangGraph şu an Neuron'da hangi feature'ları
sağlıyor? Swarm runtime LangGraph'ı tamamen replace edecek mi,
yoksa paralel mi yaşayacak? W3-04 (LangGraph streaming) hâlâ
devam ediyor mu, dondu mu?"*

Resolved answers (recorded here so per-WP authors don't
re-litigate):

- **LangGraph today**: powers exactly one scripted workflow
  ("Daily summary", `agent_runtime/workflows/daily_summary.py`).
  Triggered by `runs:create`; emits spans into `runs_spans`
  via the length-prefixed JSON sidecar protocol; no other
  workflow uses it.
- **Swarm vs. LangGraph**: **coexist, not replace**. Different
  feature shapes — LangGraph is "scripted graph runner"
  (button-triggered, fixed flow); Swarm is "şefli ekip"
  (chat-triggered, Coordinator-decided flow). Both write to the
  same SQLite (runs/spans) but the runtimes do not import each
  other. Phase 1 substrate explicitly forbids cross-imports
  (see WP-W3-11 §"Sub-agent reminders").
- **W3-04 status**: **deferred**, not frozen. LangGraph cancel +
  streaming remains technically valuable but priority drops
  because (a) Swarm gets its own cancel + streaming via
  W3-12+ as a first-class concern, (b) Daily summary is a
  30-second scripted job — no user-facing demand for cancel
  yet, (c) only one scripted workflow exists; cancel ergonomics
  matter once W3-08 ships the workflow editor and a non-trivial
  pile of long-running workflows can be authored. Re-evaluate
  W3-04 at W3-08 close. **W3-10 (Python embed) does NOT
  block on W3-04** — it is reframed as standalone-runnable so
  the bundle stays self-contained even if W3-04 sleeps.

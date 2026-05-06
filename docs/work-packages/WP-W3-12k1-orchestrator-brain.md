---
id: WP-W3-12k1
title: Orchestrator brain — stateless one-shot decision (9th bundled profile)
owner: TBD
status: not-started
depends-on: [WP-W3-12j]
acceptance-gate: "`swarm:orchestrator_decide(workspace_id, user_message) -> OrchestratorOutcome` IPC. New 9th bundled profile `orchestrator.md` makes a single-shot routing decision per user message: Clarify (return text question for user) | Dispatch (return goal to feed swarm:run_job) | DirectReply (return text answer for trivial questions). Backend + bindings only. UI integration is W3-12k-3."
---

## Goal

Land the 9th and final agent of the architectural report
§2.1's vision: Orchestrator. The Orchestrator is the
user-facing layer ABOVE Coordinator — it decides whether a
user's chat message warrants a swarm dispatch, a clarifying
question, or a direct conversational reply.

This WP ships the **stateless** Orchestrator: each call to
`swarm:orchestrator_decide` spawns a one-shot claude
subprocess with the new `orchestrator.md` persona, parses its
JSON decision, and returns it. No persistent chat history (W3-12k-2);
no UI integration (W3-12k-3). Just the brain.

After this lands, calling `orchestrator_decide` followed by
the existing `run_job` (when route=Dispatch) is a 2-IPC
sequence the frontend can wire trivially. W3-12k-2 adds
persistent context across messages; W3-12k-3 adds the chat UI.

## Why now

W3-12j completed the Coordinator FSM — 8 of 9 agents active.
The Orchestrator is the only role from architectural report
§2.1 that's still missing. With 12k-1 the swarm has a true
"şefli ekip": Orchestrator → Coordinator → specialists,
matching the report's hierarchy diagram.

User-side gain: today the user types a goal directly into the
Coordinator FSM via `swarm:run_job`. There's no
disambiguation, no "are you sure?", no follow-up clarification.
With Orchestrator, the user can chat naturally — "explain X"
gets a direct reply, "add Y" dispatches, "I'm not sure how to
phrase it..." gets a clarifying question.

## Charter alignment

No tech-stack change. The Orchestrator profile is a 9th `.md`
file in the bundled set. The decision parsing follows the
existing W3-12d (Verdict) / W3-12f (CoordinatorDecision)
robust-JSON pattern.

## Scope

### 1. New `orchestrator.md` bundled profile

`src-tauri/src/swarm/agents/orchestrator.md`:

```yaml
---
id: orchestrator
version: 1.0.0
role: Orchestrator
description: User-facing chat brain. Decides per message: clarify, dispatch to Coordinator, or direct reply.
allowed_tools: ["Read", "Grep", "Glob"]
permission_mode: plan
max_turns: 6
---
```

Body — strict prompt engineering per architectural report §7.2:

- Persona: "Sen kullanıcının dış kapısısın. Senin görevin
  kullanıcı mesajını anlamak ve üç eyleme yönlendirmek..."
- Decision rules:
  - **direct_reply** if the user is asking a conversational /
    meta question that doesn't require codebase investigation
    OR a swarm dispatch (e.g. "selam", "bugün ne yapacağız",
    "swarm nasıl çalışıyor", "merhaba"). Reply with a short
    text answer.
  - **clarify** if the user's message is too ambiguous to
    dispatch (missing target file, unclear scope, conflicting
    requirements). Return a short clarifying question to ask
    back to the user.
  - **dispatch** if the user's message is concrete enough to
    feed `swarm:run_job` directly. The Orchestrator may
    rephrase the user's message into a tighter goal (adding
    "EXECUTE:" hint, file paths, etc.) so the Coordinator
    brain classifies correctly.
- OUTPUT CONTRACT (exact JSON shape):
  ```json
  { "action": "direct_reply" | "clarify" | "dispatch",
    "text": "<reply text | clarifying question | refined goal>",
    "reasoning": "<why this action>" }
  ```
- 4 few-shot examples covering each action variant + a
  borderline case.
- Negative examples: no fence, no preamble, no multi-paragraph
  reasoning before the JSON.
- "Sen Coordinator değil sen Orchestrator'sın — kod yazma,
  Coordinator'ı çağıracak metin üret" reminder.

### 2. `swarm/coordinator/orchestrator.rs` module

```rust
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorAction {
    DirectReply,
    Clarify,
    Dispatch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct OrchestratorOutcome {
    pub action: OrchestratorAction,
    /// For DirectReply: the assistant's answer.
    /// For Clarify: the question to show the user.
    /// For Dispatch: the refined goal to feed swarm:run_job.
    pub text: String,
    pub reasoning: String,
}

pub fn parse_orchestrator_outcome(raw: &str) -> Result<OrchestratorOutcome, AppError> {
    // 4-step robust parser (mirrors parse_verdict / parse_decision):
    // 1. Direct serde_json::from_str on trimmed.
    // 2. Strip markdown fence, retry.
    // 3. First balanced {...} substring, retry.
    // 4. Err(AppError::SwarmInvoke).
}
```

Wire through `swarm/coordinator/mod.rs` (`pub mod orchestrator;
pub use orchestrator::*;`).

The parser is duplicated from verdict.rs / decision.rs (per
W3-12f's documented choice — error messages diverge,
generalization is awkward).

### 3. Tauri command `swarm:orchestrator_decide`

In `src-tauri/src/commands/swarm.rs`:

```rust
#[tauri::command]
#[specta::specta]
pub async fn swarm_orchestrator_decide<R: Runtime>(
    app: AppHandle<R>,
    workspace_id: String,
    user_message: String,
) -> Result<OrchestratorOutcome, AppError>;
```

Body:
- Validate workspace_id non-empty (else `AppError::InvalidInput`).
- Validate user_message non-empty (else `AppError::InvalidInput`).
- Load `ProfileRegistry::load_from(Some(<app_data_dir>/agents/))`.
- Get `orchestrator` profile (else `AppError::NotFound`).
- Construct `SubprocessTransport`.
- Call `transport.invoke(app, profile, &user_message, stage_timeout())`.
- Parse the InvokeResult's `assistant_text` via `parse_orchestrator_outcome`.
- Return parsed OrchestratorOutcome.

Same env / OAuth pattern as `swarm:test_invoke` from W3-11.

### 4. Bundled-profile count update

`swarm:profiles_list` now returns 9 entries (alphabetical):
- backend-builder
- backend-reviewer
- coordinator
- frontend-builder
- frontend-reviewer
- integration-tester
- orchestrator
- planner
- scout

`profile.rs::tests::bundled_eight_profiles_present` →
`bundled_nine_profiles_present` (rename + expanded id list).
`commands/swarm.rs::profiles_list_returns_eight_bundled` →
`..._nine_bundled`.

### 5. Tests (mock-driven; NO new ignored integration test)

- `parse_orchestrator_outcome_direct_reply_variant` — `{"action":"direct_reply","text":"merhaba!","reasoning":"selamlama"}` parses.
- `parse_orchestrator_outcome_clarify_variant`.
- `parse_orchestrator_outcome_dispatch_variant`.
- `parse_orchestrator_outcome_with_json_fence` — markdown-fence variant.
- `parse_orchestrator_outcome_with_preamble` — balanced-substring variant.
- `parse_orchestrator_outcome_unparseable_returns_error`.
- `parse_orchestrator_outcome_unknown_action_rejected` — `"action":"do_x"` → SwarmInvoke.
- `orchestrator_action_serializes_snake_case`.
- `swarm_orchestrator_decide_command_validates_empty_workspace_id`.
- `swarm_orchestrator_decide_command_validates_empty_message`.
- `swarm_orchestrator_decide_command_returns_outcome_via_mock_transport` — using a MockTransport with canned orchestrator response.
- `bundled_nine_profiles_present`.
- `bundled_nine_profiles_have_distinct_ids`.

NO real-claude integration smoke for this WP. The mock tests
cover the parser + command surface; an integration smoke would
just verify "claude is alive" which W3-11/12d/etc already prove.
W3-12k-3 (UI) is where end-to-end orchestrator usage gets
validated.

### 6. Bindings regen

`pnpm gen:bindings` adds:
- `OrchestratorAction` enum (`direct_reply` | `clarify` | `dispatch`)
- `OrchestratorOutcome` struct
- `commands.swarmOrchestratorDecide(workspaceId, userMessage)`

`pnpm gen:bindings:check` exits 0 post-commit.

### 7. UI follow-up note

Frontend integration is W3-12k-3 (separate WP). After 12k-1
ships, frontend can:
1. Call `commands.swarmOrchestratorDecide(workspace, message)`
2. Branch on `outcome.action`:
   - `direct_reply` → show `outcome.text` as orchestrator's answer
   - `clarify` → show `outcome.text` as a question to user
   - `dispatch` → call `commands.swarmRunJob(workspace, outcome.text)` to start a swarm job
3. Existing W3-14 SwarmJobDetail/List UI handles the dispatched job

12k-2 adds persistent context (conversation history) across
multiple `orchestrator_decide` calls. 12k-1 is stateless: each
call is independent.

## Out of scope

- ❌ Persistent chat history. Each call to
  `orchestrator_decide` is independent. W3-12k-2 adds session
  persistence + history-aware decisions.
- ❌ UI chat panel. W3-12k-3.
- ❌ Auto-dispatch on Dispatch outcome. The IPC returns the
  refined goal; frontend explicitly calls `swarm:run_job`.
  Future polish could combine into one IPC call but staying
  separate keeps the surface composable.
- ❌ Multi-workspace orchestrator routing.
  `swarm:orchestrator_decide` takes workspace_id but the
  Orchestrator persona doesn't differentiate; future polish.
- ❌ Streaming orchestrator response. One-shot, returns full
  text in one IPC.

## Acceptance criteria

- [ ] `swarm/agents/orchestrator.md` exists, embedded via include_dir!.
- [ ] `swarm:profiles_list` returns 9 entries (alphabetical).
- [ ] `swarm/coordinator/orchestrator.rs` module exists with
      `OrchestratorAction` enum, `OrchestratorOutcome` struct,
      `parse_orchestrator_outcome` 4-step robust parser.
- [ ] `swarm:orchestrator_decide(workspace_id, user_message)`
      Tauri command compiles, types end-to-end.
- [ ] All Week-2 + Week-3-prior tests pass; target ≥360 unit.
- [ ] No new dep, no new migration, no `unsafe`, no `eprintln!`.
- [ ] `bindings.ts` regenerated with `swarmOrchestratorDecide`,
      `OrchestratorAction`, `OrchestratorOutcome`.
- [ ] `pnpm gen:bindings:check` exits 0 post-commit.

## Verification commands

```bash
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

pnpm gen:bindings
pnpm gen:bindings:check    # exit 1 pre-commit, 0 post
pnpm typecheck
pnpm test --run
pnpm lint
```

NO new real-claude integration smoke for this WP (mock tests
sufficient).

## Notes / risks

- **Borderline goals.** "Add a doc comment to X" could be
  Dispatch OR DirectReply (if X already has the comment). The
  Orchestrator persona's heuristic should default to Dispatch
  when in doubt (failing toward action over explanation).
- **Refined goals from Orchestrator.** Orchestrator may
  rewrite user's "Add docs" to "EXECUTE: Edit src-tauri/...";
  this should help Coordinator's downstream classification
  (which we saw in W3-12i smoke can be persona-flaky on
  ambiguous goals).
- **No conversation memory.** A user typing two messages back-
  to-back gets two independent Orchestrator decisions. "I want
  to refactor auth" → Clarify ("which file?"). User: "the
  Tauri command in commands/auth.rs" → without history, the
  second message looks like a free-floating file mention. The
  frontend can manually concatenate previous messages to
  preserve context, OR W3-12k-2's persistent session handles
  this naturally.
- **Cost note.** Each `orchestrator_decide` is a single LLM
  call (~$0.005-0.02). Cheap relative to a full
  Coordinator+specialist run. User directive 2026-05-06 says
  cost not a concern.

## Sub-agent reminders

- Read this WP in full.
- Read `swarm/agents/coordinator.md` for the existing
  decision-emitting persona pattern; orchestrator.md mirrors
  the structure.
- Read `swarm/coordinator/{decision,verdict}.rs` for the
  robust JSON parser pattern. Duplicate (per W3-12f's
  documented choice).
- Read `swarm/commands/swarm.rs` for the existing IPC
  pattern (`swarm:test_invoke`, `swarm:run_job`); mirror it.
- DO NOT add a new dep.
- DO NOT add persistence for orchestrator state. Stateless
  per W3-12k-1 contract.
- DO NOT add a new SwarmJobEvent variant — orchestrator_decide
  is one-shot, not a long-running job.
- DO NOT integrate with frontend. W3-12k-3 territory.
- DO NOT change Coordinator brain or FSM behavior.
- Per AGENTS.md: one WP = one commit.

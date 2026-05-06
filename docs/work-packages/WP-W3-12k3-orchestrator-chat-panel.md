---
id: WP-W3-12k3
title: Orchestrator chat panel UI (replaces SwarmGoalForm)
owner: TBD
status: not-started
depends-on: [WP-W3-12k1]
acceptance-gate: "Sidebar's Swarm route's left pane replaces SwarmGoalForm with a chat-shape OrchestratorChatPanel. User types a message, hits Enter; frontend calls `swarm:orchestrator_decide`; renders DirectReply/Clarify as bot bubbles; on Dispatch, automatically calls `swarm:run_job` and shows 'Started job …' with a click-through to the job detail. Chat history is local component state (no persistence — W3-12k-2 adds it). Existing recent-jobs list + SwarmJobDetail unchanged."
---

## Goal

Make the 9th agent (Orchestrator, shipped backend in W3-12k-1)
user-visible. Replace the W3-14 SwarmGoalForm with a chat-shaped
OrchestratorChatPanel. The user types natural-language messages;
Orchestrator decides per message whether to chat back, ask a
question, or kick off a swarm job.

This is the LAST UI WP that completes the 9-agent vision's
user-facing surface. After 12k-3, persistence (12k-2) is the
quality-of-life addition.

## Why now

W3-12k-1 shipped the Orchestrator brain on the IPC layer
(`swarm:orchestrator_decide`). Without UI, that IPC is
DevTools-only — same situation W3-12c was in before W3-14.

The user said 2026-05-06: "9 agent ekibi bu yüzden istiyorum
zaten." Ekibin 9. ajanı brain'i ile yeterli değil — kullanıcı
görmeli, etkileşmeli. UI is the difference between "feature
ships" and "feature lives."

## Charter alignment

No tech-stack change. New React component + hook; existing
TanStack Query + Tauri invoke patterns.

## Scope

### 1. Replace `SwarmGoalForm` with `OrchestratorChatPanel`

`app/src/components/OrchestratorChatPanel.tsx` — new component.

Layout:
```
┌───────────────────────────────────┐
│  [chat history scroll area]       │
│  ┌───────────────────────────┐    │
│  │ user: "Add doc to X.tsx"  │    │ (right-aligned bubble)
│  └───────────────────────────┘    │
│  ┌───────────────────────────┐    │
│  │ orchestrator: "✓ Started   │    │ (left-aligned bubble)
│  │ job a-1234. View details ↗" │    │
│  └───────────────────────────┘    │
│  ...                              │
├───────────────────────────────────┤
│  [textarea]               [Send]  │
└───────────────────────────────────┘
```

Local state (useState):
- `messages: ChatMessage[]` — append-only.
- `input: string` — current textarea value.

`ChatMessage` shape:
```typescript
type ChatMessage =
  | { role: 'user'; text: string; ts: number }
  | { role: 'orchestrator'; action: OrchestratorAction; text: string; reasoning: string; ts: number }
  | { role: 'job'; jobId: string; goal: string; ts: number };
```

When user submits:
1. Append user message to history.
2. Call `swarm:orchestrator_decide(workspace, userText)`.
3. On Ok:
   - Append orchestrator message with the outcome.
   - If `action === 'dispatch'`: also call `swarm:run_job(workspace, outcome.text)`, append a `{role: 'job', jobId, goal: outcome.text}` message.
   - On `direct_reply` or `clarify`: just the orchestrator bubble; user can reply.
4. On Err: show error inline (red banner above input).

The `SwarmRoute` component (existing) replaces its
`<SwarmGoalForm />` import with `<OrchestratorChatPanel />`.

### 2. New hook `useOrchestratorDecide`

`app/src/hooks/useOrchestratorDecide.ts`:

```typescript
import { useMutation } from '@tanstack/react-query';
import { commands, type OrchestratorOutcome } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

export function useOrchestratorDecide() {
  return useMutation<
    OrchestratorOutcome,
    Error,
    { workspaceId: string; userMessage: string }
  >({
    mutationFn: ({ workspaceId, userMessage }) =>
      unwrap(commands.swarmOrchestratorDecide(workspaceId, userMessage)),
  });
}
```

Mirrors `useRunSwarmJob.ts` pattern.

### 3. `useRunSwarmJob` reuse

`OrchestratorChatPanel` uses BOTH hooks:
- `useOrchestratorDecide()` for the user-message → outcome step.
- `useRunSwarmJob()` (existing) for the dispatch follow-up.

Both fire from the same submit handler in sequence.

### 4. Job-link click-through

When the chat shows a `{role: 'job', jobId, ...}` message, it's
a clickable link. Clicking sets the parent `SwarmRoute`'s
selectedJobId state to that jobId. Right pane (SwarmJobDetail)
renders that job's detail.

`OrchestratorChatPanel` takes a prop `onSelectJob: (jobId:
string) => void` from SwarmRoute. The job-message bubble's
button calls this prop.

### 5. CSS

`app/src/styles/swarm.css` — add chat-panel layout:

- `.swarm-chat` — container (flex column, full height).
- `.swarm-chat-history` — scroll area (flex 1, overflow-y auto).
- `.swarm-chat-msg` — message bubble base.
- `.swarm-chat-msg.user` — right-aligned, accent background.
- `.swarm-chat-msg.orchestrator` — left-aligned, surface-2 background.
- `.swarm-chat-msg.orchestrator.dispatch` — slightly different tint (or chip) so user sees "this kicked off a job."
- `.swarm-chat-msg.job` — special pill-style with click target.
- `.swarm-chat-input-row` — textarea + Send button at bottom.
- `.swarm-chat-error` — red banner above input.

Reuse design-system tokens (`var(--accent)`, `var(--surface-2)`,
`var(--syn-error)`, etc.) per Charter §"Hard constraints" #4.

### 6. Empty / loading states

- **Empty chat**: brief explainer ("Chat with the Swarm
  Orchestrator. Ask questions or describe what you want to
  build.") in the history area when `messages.length === 0`.
- **Loading**: while `useOrchestratorDecide().isPending`, show
  a "thinking…" bubble (greyed out, animated dots). Disable
  the Send button.
- **Dispatch in flight**: while `useRunSwarmJob().isPending`,
  show "Starting job…" inline. The job appears in the recent-
  jobs list automatically (W3-14 pattern) via the existing
  invalidation.

### 7. Tests (Vitest)

`app/src/components/OrchestratorChatPanel.test.tsx`:

- `renders empty state` — initial render with no messages shows
  the explainer.
- `appends user message on submit` — type + click Send; user
  bubble appears; mutation called with correct args.
- `renders direct_reply outcome as orchestrator bubble`.
- `renders clarify outcome as orchestrator bubble`.
- `dispatch outcome triggers run_job and appends job message` —
  mock both mutations; user types; orchestrator dispatches; job
  message appears with the returned jobId.
- `error state shown when orchestrator_decide fails`.
- `clicking job message calls onSelectJob with jobId`.
- `disabled state during pending mutation`.

`app/src/hooks/useOrchestratorDecide.test.tsx`:

- `mutationFn calls swarmOrchestratorDecide with workspaceId and message` — mock the IPC, assert args + return.

Existing W3-14 tests (`SwarmRoute.test.tsx`) need updating —
they reference `SwarmGoalForm`. Since we're replacing it with
OrchestratorChatPanel, the test's submit-handler shape changes.
Update to test the new flow.

Frontend test count target: 34 (current) → 42 minimum (+8).

### 8. Bindings

NO new bindings. The W3-12k-1 surface (`swarmOrchestratorDecide`,
`OrchestratorAction`, `OrchestratorOutcome`) is already in
`bindings.ts`.

`pnpm gen:bindings:check` exits 0 (no Rust changes).

### 9. SwarmRoute integration

`app/src/routes/SwarmRoute.tsx`:

- Remove `<SwarmGoalForm />` import + usage.
- Import + render `<OrchestratorChatPanel onSelectJob={setSelectedJobId} />`.
- The right pane (SwarmJobDetail with selectedJobId) is
  unchanged.
- The recent-jobs list below the chat is unchanged.

Alternative layout question: chat replaces the goal form
(top of left pane), keeping the recent-jobs list below. That
matches existing 2-pane layout. Pick this — minimal layout
disruption.

## Out of scope

- ❌ Persistent chat history. W3-12k-2: SQLite-backed
  conversation log per workspace.
- ❌ Context-aware Orchestrator decisions (multi-message
  reasoning). 12k-1 is stateless; 12k-2 wires history into the
  prompt. UI in 12k-3 is functional even without context —
  user just rephrases if Orchestrator misunderstood.
- ❌ Streaming Orchestrator response. One-shot per message.
- ❌ Multi-workspace chat panel switching (workspace_id is
  hardcoded `"default"` per W3-14 pattern).
- ❌ Markdown rendering in chat bubbles (plain text for now).
- ❌ Chat clear / reset button. Reload page = clear (until
  W3-12k-2 persistence makes refresh preserve history).
- ❌ Editing previous messages.
- ❌ Stop / cancel orchestrator mid-decision (W3-12k-1 is
  one-shot, completes in ~5-10s).

## Acceptance criteria

- [ ] `OrchestratorChatPanel.tsx` component exists.
- [ ] `useOrchestratorDecide.ts` hook exists.
- [ ] `SwarmRoute.tsx` uses `OrchestratorChatPanel` instead of
      `SwarmGoalForm`.
- [ ] Chat panel renders user/orchestrator/job message bubbles.
- [ ] Dispatch outcome triggers `useRunSwarmJob` and shows the
      resulting jobId as a clickable message.
- [ ] Click on job message calls `onSelectJob(jobId)` so
      SwarmJobDetail (right pane) loads that job.
- [ ] Empty / loading / error states render correctly.
- [ ] CSS classes added to `swarm.css`; design-system tokens
      only (no hex/HSL).
- [ ] Frontend tests pass; target ≥42 (34 + 8 new).
- [ ] No new dep on the JS side.
- [ ] `pnpm typecheck`, `pnpm test --run`, `pnpm lint` exit 0.
- [ ] No backend changes (Rust regression: 364/0/12 unchanged).

## Verification commands

```bash
pnpm typecheck
pnpm test --run
pnpm lint

# Backend regression (no changes expected):
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Manual UI smoke (orchestrator-driven post-commit, OR owner-driven):
pnpm tauri dev
# In the app:
#   1. Click "Swarm" in sidebar.
#   2. Type "selam" → expect orchestrator direct_reply.
#   3. Type "Auth refactor yap" → expect clarify (asks a follow-up).
#   4. Type "EXECUTE: Add a comment to X" → expect dispatch + job appears.
#   5. Click the job message → SwarmJobDetail (right pane) loads it.
```

## Notes / risks

- **Stateless = user has to repeat context.** A user who said
  "I want to refactor auth" then types "/me endpoint" gets two
  independent Orchestrator decisions — the second message
  doesn't know the auth context. W3-12k-2 fixes this. For now,
  guidance: tell users (in the empty-state explainer) that
  each message is independent.
- **Orchestrator output unparseable** → IPC returns
  `AppError::SwarmInvoke`. The UI shows it as an error banner.
  User can retry.
- **Dispatch-then-run race.** Between `orchestrator_decide`
  returning Dispatch and `run_job` starting, ~100ms gap. UI
  shows "Starting job…" during this. Acceptable; users won't
  notice.
- **Long Orchestrator response.** If the persona produces a
  multi-sentence DirectReply, the bubble could be tall.
  Acceptable; chat history scrolls.
- **No markdown rendering.** Plain text in bubbles.
  Orchestrator persona instructed to keep responses short and
  prose-only; if user wants formatted output they explicitly
  ask via Dispatch (which kicks off Builders that produce
  formatted output).
- **Chat history lost on reload.** Reload = empty chat. User
  must re-prompt. W3-12k-2 fixes via persistence.

## Sub-agent reminders

- Read this WP in full.
- Read `app/src/components/SwarmGoalForm.tsx` for the existing
  pattern being replaced.
- Read `app/src/hooks/{useRunSwarmJob,useCancelSwarmJob}.ts` for
  the mutation hook pattern; new `useOrchestratorDecide` mirrors it.
- Read `app/src/styles/swarm.css` for the existing layout +
  design-system token usage.
- Read `app/src/routes/SwarmRoute.tsx` for the parent
  integration point.
- DO NOT add a new JS dep. Use existing TanStack Query + Tauri
  invoke + React 18.
- DO NOT touch backend code (no Rust changes in this WP).
- DO NOT touch `bindings.ts`. The W3-12k-1 entries are already
  there.
- DO NOT add persistence. Local component state only.
  W3-12k-2 territory.
- DO NOT add streaming. One-shot per message.
- DO NOT add multi-workspace UI. workspace_id stays `"default"`.
- Per AGENTS.md: one WP = one commit.

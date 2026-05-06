---
id: WP-W3-14
title: Swarm UI route — chat-driven multi-agent run surface
owner: TBD
status: not-started
depends-on: [WP-W3-12a, WP-W3-12b, WP-W3-12c]
acceptance-gate: "A new `Swarm` route in the sidebar opens a 2-pane layout: left = job list (recent jobs from `swarm:list_jobs`), right = job detail (stages + live state). User can type a goal, click Run, see live state transitions via the W3-12c event channel, and click Cancel mid-job. Restart preserves the visible history."
---

## Goal

Make the swarm runtime usable from the UI. Today the only way
to drive `swarm:run_job` is DevTools or `cargo test
--ignored`. This WP wires the backend the W3-12 series shipped
into a chat-shaped surface that a regular user can operate.

## Why now

W3-12a/b/c finished the backend. Without UI, the feature is
DevTools-only — not shippable. W3-14 is what turns "verified
substrate" into "demo-able feature."

## Scope

### 1. New sidebar entry + route shell

`app/src/App.tsx`:
- `type Route` gains `'swarm'`.
- `NAV` gains `{ id: 'swarm', label: 'Swarm', icon: 'bot' }` (reuse the existing `bot` icon — same as Agents). Place it between `terminal` and `agents`.
- `TOPBAR_TITLE` gets `swarm: 'Swarm'`.
- `RouteHost` adds the `swarm` case wrapping `<SwarmRoute />` in `<ErrorBoundary>`.

The Topbar's "+ New" button does nothing on the swarm route in
this WP; the goal-input form is inline on the route itself.
Keep the topbar as-is.

### 2. New route component

`app/src/routes/SwarmRoute.tsx` — 2-pane layout matching the
existing `RunsRoute.tsx` structure:

```
┌───────────────────────────┬─────────────────────────────┐
│  Left pane                │  Right pane                 │
│  ─────────                │  ──────────                 │
│  [goal input + Run btn]   │  Selected job header        │
│  ─── Recent jobs ─────    │  ─── Stages ───────         │
│  • job-1 (Done, 1m ago)   │  ✓ Scout   2.4s  $0.01      │
│  • job-2 (Failed, 3m ago) │    [assistant text]         │
│  • job-3 (Running…)  ⌧    │  ✓ Plan    3.1s  $0.02      │
│                           │    [assistant text]         │
│                           │  ⊙ Build   running…         │
│                           │  ──────────────────────     │
│                           │  [Cancel] [Rerun]           │
└───────────────────────────┴─────────────────────────────┘
```

The "Rerun" button is a basic retry surface — fires another
`swarm:run_job` with the same goal + workspace_id. Disabled on
non-Failed jobs (Done jobs don't need rerun in 12a's mental
model — the user can craft a new goal). W3-12d will add a
richer retry-with-feedback variant.

The "Cancel" button is enabled iff the selected job's
`final_state` (or transient `state` from the live event stream)
is non-terminal. Calls `swarm:cancel_job(job_id)`.

`workspace_id` for this WP is the constant `"default"` (matches
the W3-12a/b smoke pattern). Multi-workspace UX is post-W3.

### 3. New hooks

`app/src/hooks/useSwarmJobs.ts`:

```typescript
export function useSwarmJobs(workspaceId?: string, limit?: number) {
  return useQuery<JobSummary[]>({
    queryKey: ['swarm-jobs', workspaceId ?? null, limit ?? null],
    queryFn: () => unwrap(commands.swarmListJobs(workspaceId ?? null, limit ?? null)),
    refetchInterval: 5_000,  // Light polling so list stays fresh; events
                              // invalidate the cache for instant updates.
  });
}
```

`app/src/hooks/useSwarmJob.ts`:

```typescript
export function useSwarmJob(jobId: string | null) {
  const queryClient = useQueryClient();
  // Subscribe to the per-job event channel; on each event,
  // optimistically update the cache so the UI reflects the
  // live state without waiting for the next poll.
  useEffect(() => {
    if (!jobId) return;
    const channel = `swarm:job:${jobId}:event`;
    const unlistenP = listen<SwarmJobEvent>(channel, (evt) => {
      queryClient.setQueryData<JobDetail>(['swarm-job', jobId], (prev) => {
        if (!prev) return prev;
        return applySwarmEventToJobDetail(prev, evt.payload);
      });
      if (evt.payload.kind === 'finished') {
        queryClient.invalidateQueries({ queryKey: ['swarm-jobs'] });
      }
    });
    return () => { void unlistenP.then((u) => u()); };
  }, [jobId, queryClient]);

  return useQuery<JobDetail>({
    queryKey: ['swarm-job', jobId],
    queryFn: () => unwrap(commands.swarmGetJob(jobId!)),
    enabled: !!jobId,
  });
}
```

`applySwarmEventToJobDetail` is a pure helper:
- `started` → no-op (data already in cache)
- `stage_started` → set `state` to the new stage
- `stage_completed` → push the stage onto `stages`, advance `state`
- `finished` → replace with `outcome`'s fields
- `cancelled` → no-op (the subsequent `finished` carries the terminal state)

`app/src/hooks/useRunSwarmJob.ts`:

```typescript
export function useRunSwarmJob() {
  const queryClient = useQueryClient();
  return useMutation<JobOutcome, AppError, { workspaceId: string; goal: string }>({
    mutationFn: ({ workspaceId, goal }) =>
      unwrap(commands.swarmRunJob(workspaceId, goal)),
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: ['swarm-jobs'] });
    },
  });
}
```

`app/src/hooks/useCancelSwarmJob.ts`:

```typescript
export function useCancelSwarmJob() {
  const queryClient = useQueryClient();
  return useMutation<void, AppError, string /* jobId */>({
    mutationFn: (jobId) => unwrap(commands.swarmCancelJob(jobId)),
    onSettled: (_, __, jobId) => {
      queryClient.invalidateQueries({ queryKey: ['swarm-job', jobId] });
    },
  });
}
```

### 4. Component breakdown

`app/src/components/SwarmGoalForm.tsx`:
- Textarea for goal, "Run" button.
- Disabled while `useRunSwarmJob().isPending`.
- On submit: clears the textarea, the new job auto-appears in the list (via `onSettled` invalidation).

`app/src/components/SwarmJobList.tsx`:
- Renders `useSwarmJobs(workspaceId)`'s data.
- Each row: status pill (running / done / failed), goal preview (200 chars from `JobSummary`), elapsed time.
- Click → set selected job_id (passed up via prop callback).
- "running…" rows show a small spinner.

`app/src/components/SwarmJobDetail.tsx`:
- Header: goal (full text from `JobDetail.goal`), state pill, total cost.
- Stage list: one row per `StageResult`. Show specialist_id, duration, cost, expandable assistant_text (truncated to 600 chars in the row, expand on click).
- Footer: `[Cancel]` (if non-terminal) + `[Rerun]` (if Failed) buttons.
- Empty state: "Select a job from the left."

The components are thin. The hooks own the data lifecycle.

### 5. Styles

`app/src/styles/swarm.css` — minimal, mirrors the existing
`runs.css` conventions (status-pill colors, list-row layout).
Reuse `.btn`, `.pill`, `.list-row` tokens from the design
system; only the layout grid is new.

### 6. Tests

Frontend Vitest tests:

- `useSwarmJobs.test.tsx` — TanStack Query mock; assert the
  list re-fetches on a `swarm:job:*:event` of kind `finished`.
- `useSwarmJob.test.tsx` — assert the `applySwarmEventToJobDetail`
  helper updates state on each event kind correctly.
- `SwarmRoute.test.tsx` — render the route with a mocked
  `JobSummary[]` and `JobDetail`; assert the goal-input form
  fires `commands.swarmRunJob` on submit; assert the Cancel
  button only renders for non-terminal jobs; assert clicking a
  list row updates the right pane.
- `SwarmJobDetail.test.tsx` — render with a fixture detail;
  assert all stages show; assert the Rerun button only shows
  for Failed jobs.

Mock the Tauri `commands.*` and `listen` functions (vitest +
existing `app/src/test/setup.ts` patterns).

Target frontend test count: 17 prior + 8 new = 25 minimum.

### 7. Bindings

No new bindings — W3-12a/b/c shipped them all (`swarmRunJob`,
`swarmCancelJob`, `swarmListJobs`, `swarmGetJob`,
`SwarmJobEvent`, `JobSummary`, `JobDetail`, `JobOutcome`,
`StageResult`, `JobState`).

## Out of scope

- ❌ Multi-workspace UX (workspace picker). Workspace is hardcoded `"default"`. Multi-workspace is post-W3.
- ❌ Pagination beyond the 200-row IPC cap. Infinite scroll is post-W3.
- ❌ Specialist pane streaming with per-agent stdout (the architectural report's §8.2 multi-pane vision). The W3-14 surface is single-pane chat-style; the §8.2 multi-pane is a future polish WP if owner wants it.
- ❌ Token-level streaming (assistant message deltas mid-stage). Stage-level only (W3-12c contract).
- ❌ Profile editor UI. `swarm:profiles_list` IPC exists but the UI doesn't expose it in 14; profiles are read-only bundled. Profile editor is post-W3.
- ❌ Cost / budget meter (cumulative spend across jobs). Per-job cost is shown in the detail; aggregation is post-W3.
- ❌ Search / filter on job list.
- ❌ Inspector view on the run panel (the existing `RunInspector` is for LangGraph runs, not swarm jobs).

## Acceptance criteria

- [ ] `Swarm` sidebar entry visible; clicking it loads the new route
- [ ] Goal-input form fires `swarm:run_job(workspaceId="default", goal)` on submit
- [ ] Recent jobs list populates from `swarm:list_jobs`
- [ ] Selecting a list row populates the right detail pane via `swarm:get_job`
- [ ] Live state updates: while a job runs, the right pane reflects state transitions in <1s of the event firing
- [ ] Cancel button visible iff selected job is non-terminal; clicking it fires `swarm:cancel_job` and the UI flips to Failed within 2s
- [ ] Rerun button visible iff selected job is Failed; clicking it fires a fresh `swarm:run_job` with the same goal
- [ ] App restart preserves the recent-jobs list (sourced from SQLite via W3-12b)
- [ ] All Week-2 + Week-3-prior frontend tests still pass; target ≥25 frontend tests
- [ ] No new dep on the JS side
- [ ] `pnpm typecheck`, `pnpm test --run`, `pnpm lint` all exit 0

## Verification commands

```bash
pnpm typecheck
pnpm test --run
pnpm lint

# Rust gates (regression — no Rust changes, but run anyway):
cargo check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml --lib

# Manual UI smoke (orchestrator-driven, post-commit):
pnpm tauri dev
# Then in the running app:
#   1. Click "Swarm" in sidebar
#   2. Type a goal, click Run
#   3. Watch state transitions live
#   4. Mid-Build, click Cancel; observe Failed status flip
#   5. Click Rerun; observe new job in list
#   6. Close app, reopen; confirm jobs list still shows history
```

## Notes / risks

- **`listen` cleanup races.** `useEffect` returns a cleanup that awaits `unlistenP`. If the component unmounts before the listen call resolves, the cleanup awaits the resolved unlisten — fine. The classic React StrictMode double-invoke is handled by re-listening inside `useEffect`.
- **Optimistic cache update vs. server truth.** When `stage_started` fires, the hook bumps `state` to the new stage; if the user happens to refresh `swarm:get_job` at the same moment, the server-fresh data may briefly say a different state. Acceptable — the next event corrects it.
- **Cost accumulation across stages.** `JobDetail.total_cost_usd` is the sum across `stages`. The live `stage_completed` event carries a single stage's cost; the hook's helper increments the running total. Visible to user as a cost ticker.
- **Cancel race**. User clicks Cancel during the gap between `stage_completed(Plan)` and `stage_started(Build)`. Backend handles this (W3-12c documented behavior); UI just shows the eventual `cancelled_during: Build` from the `cancelled` event then `Failed` from `finished`. No special UI logic needed.
- **No streaming UI for individual agent stdout.** The detail pane shows `StageResult.assistant_text` post-stage-completion. While a stage runs, the UI shows "running…" with no mid-stage hint. Token-level streaming is explicitly out of scope.
- **Manual smoke is owner-driven post-commit.** The headless E2E (Playwright + Tauri WebDriver) is W3-09 territory; this WP doesn't add automated UI smoke beyond Vitest unit tests.

## Sub-agent reminders

- Read this WP in full before writing code.
- Read `app/src/hooks/useRuns.ts`, `useRun.ts`, `usePanes.ts` for the existing hook patterns to mirror.
- Read `app/src/routes/RunsRoute.tsx` for the existing 2-pane layout convention.
- Read `app/src/styles/runs.css` for the design-system token usage.
- DO NOT add a new JS dep — TanStack Query, React 18, Tauri's `@tauri-apps/api/event` (for `listen`) are all already in tree.
- DO NOT change any backend code (`src-tauri/`). This WP is frontend-only.
- DO NOT introduce a workspace picker. `workspaceId = "default"` is the contract.
- DO NOT regenerate `bindings.ts`. The bindings were shipped by W3-12a/b/c; this WP only consumes them.
- Per `AGENTS.md`: one WP = one commit.

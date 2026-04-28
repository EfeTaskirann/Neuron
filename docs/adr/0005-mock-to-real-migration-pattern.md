---
id: ADR-0005
title: Mock-to-real migration pattern
status: accepted
date: 2026-04-21
deciders: Efe Taşkıran
---

## Context

WP-W2-08 must migrate every UI component from `window.NeuronData` to a real backend without changing component code structure. We need a single repeatable pattern so the migration is mechanical and reviewable.

## Decision

**Replace data source, never data shape.** Use TanStack Query hooks per top-level NeuronData key. Component bodies stay identical except for the data acquisition line.

### Pattern

For each top-level key `X` in `window.NeuronData`:

1. **Identify** — note the shape of `data.X` in the mock.
2. **Backend command** — confirm a Tauri command produces matching shape (`X:list` or similar). If shape diverges, transform in the Rust handler, not in the React hook.
3. **Hook** — create `app/src/hooks/useX.ts`:
   ```ts
   export function useX() {
     return useQuery({
       queryKey: ['X'],
       queryFn: () => invoke<X[]>('X:list'),
     });
   }
   ```
4. **Component diff**:
   ```diff
   -const data = window.NeuronData;
   -const items = data.X;
   +const { data: items = [], isLoading, isError } = useX();
   ```
5. **Empty/error states** — wrap in `<ErrorBoundary>` (route-level) and handle `items.length === 0` (component-level).
6. **Mutations** — `useXCreate`, `useXUpdate`, `useXDelete` invalidate `['X']` on success.

## Rationale

- **Reviewable diffs.** Every PR shows a tight diff: import + hook + 1-line replacement.
- **Frontend invariant.** Components don't know what backend produces; just call a hook. Backend changes don't ripple to component logic.
- **Cache + invalidation for free.** TanStack Query handles refetch, mutation invalidation, retry-on-mount.
- **Empty/error states as first-class.** Forces every list to handle "nothing here yet" + "couldn't fetch".
- **Piecewise migration.** AgentsRoute can ship migrated before RunsRoute. Each route is independently deployable.

## Consequences

- ✅ Backend can ship piecewise (migrate AgentsRoute first; if `agents:list` returns `[]` from empty table, the empty state renders correctly)
- ✅ Every list route has identical structure — less cognitive load when reviewing PRs
- ⚠️ Backend must produce shapes matching the mock exactly. Any drift surfaces as TypeScript errors after WP-W2-03's `bindings.ts` is generated
- ⚠️ Some mock fields are computed/derived (e.g., a formatted timestamp). Backend must produce them or hooks must compute. **Default: backend computes**, hook returns ready-to-render

## Forbidden patterns

- ❌ Adding a new top-level key in `data.js` for backend convenience
- ❌ Reshaping a list at the component level (`items.map(adapter)`) — adapt in the hook or backend
- ❌ Calling `invoke()` directly inside a component (always go through a hook)
- ❌ Bypassing TanStack Query (no manual `useEffect + fetch`)

## Live updates (events, not polling)

For real-time data (run spans, terminal output), the hook also subscribes to Tauri events:

```ts
export function useRun(runId: string) {
  const qc = useQueryClient();
  const query = useQuery({ queryKey: ['runs', runId], queryFn: () => invoke('runs:get', { id: runId }) });
  useEffect(() => {
    const unlisten = listen(`run.${runId}.span`, (e) => {
      qc.setQueryData(['runs', runId], (old) => mergeSpan(old, e.payload));
    });
    return () => { unlisten.then(fn => fn()); };
  }, [runId, qc]);
  return query;
}
```

The cache is the single source of truth; events are merged into the cache, components re-render automatically.

## Revisit

If a Week 3+ requirement makes the 1-key-per-hook ratio awkward (e.g., a "Daily summary" page combining 5 lists), introduce a `useDashboard()` aggregating hook that returns multiple keys, but each underlying source still goes through its own hook. Don't let "dashboard hooks" become a backdoor for shape mutation.

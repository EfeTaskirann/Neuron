// `useRun(id)` — full run detail + live span updates per ADR-0006.
// The query fetches the snapshot once via `runs:get`; thereafter
// each `runs:{id}:span` event merges into the cache through
// `qc.setQueryData`, so component re-renders are driven by the
// cache rather than a parallel state tree.
//
// Span events arrive as `{ kind: 'created'|'updated'|'closed', span }`.
// Created → push; updated/closed → replace the row by id. The
// hook keeps spans sorted by `t0Ms` ASC because that's how the
// inspector renders them; merging out-of-order events doesn't
// break the contract.
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { commands, type RunDetail, type Span } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';
import { useTauriEvent } from './useTauriEvent';

interface SpanEventPayload {
  kind: 'created' | 'updated' | 'closed';
  span: Span;
}

export function useRun(id: string | null | undefined) {
  const qc = useQueryClient();
  const query = useQuery<RunDetail>({
    queryKey: ['run', id],
    queryFn: () => unwrap(commands.runsGet(id as string)),
    enabled: !!id,
  });

  useTauriEvent<SpanEventPayload>(id ? `runs:${id}:span` : null, (payload) => {
    qc.setQueryData<RunDetail>(['run', id], (prev) => {
      if (!prev) return prev;
      const incoming = payload.span;
      const existing = prev.spans.findIndex((s) => s.id === incoming.id);
      let nextSpans: Span[];
      if (payload.kind === 'created' && existing === -1) {
        nextSpans = [...prev.spans, incoming];
      } else if (existing !== -1) {
        nextSpans = prev.spans.slice();
        // Merge: backend may emit a closed span that flips
        // is_running false + sets durationMs; preserve existing
        // indent (computed by runs:get CTE; events ship 0).
        nextSpans[existing] = { ...nextSpans[existing], ...incoming, indent: nextSpans[existing].indent };
      } else {
        nextSpans = [...prev.spans, incoming];
      }
      nextSpans.sort((a, b) => a.t0Ms - b.t0Ms);
      return { ...prev, spans: nextSpans };
    });
  });

  return query;
}

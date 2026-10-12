// Logic layer for `OrchestratorChatPanel` (WP-W3-12k2). Owns the chat
// state machine — persisted-history seed, local bubbles, the
// decide → dispatch → log submit chain, and clear — so the panel
// component stays pure presentation. Extracted per BACKLOG T2-02
// (sunum/efekt ayrımı); behaviour is identical to the pre-split
// component.
//
// W3-12k1 shipped the stateless brain. W3-12k3 shipped the chat UI
// with React-only state. W3-12k2 (this layer) adds SQLite persistence:
//
// - On mount: `useOrchestratorHistory(workspaceId)` reads the persisted
//   thread and seeds `seedMessages`.
// - Submit chains decide → (dispatch ? run_job → log_job) and appends
//   bubbles to `localMessages` so the echo is immediate.
// - We deliberately do NOT invalidate `['orchestrator-history']`
//   mid-session (that would refetch the freshly persisted rows and
//   double every bubble); the next mount re-runs the query instead.
// - "Clear chat" calls `useClearOrchestratorHistory`, suppresses the
//   seed, drops local bubbles, and invalidates so the next mount sees
//   the empty thread.
import { useState, useRef, useEffect, useMemo, type RefObject } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type {
  OrchestratorAction,
  OrchestratorMessage,
  OrchestratorOutcome,
} from '../lib/bindings';
import { useOrchestratorDecide } from './useOrchestratorDecide';
import { useRunSwarmJob } from './useRunSwarmJob';
import { useOrchestratorHistory } from './useOrchestratorHistory';
import { useClearOrchestratorHistory } from './useClearOrchestratorHistory';
import { useLogOrchestratorJob } from './useLogOrchestratorJob';

export type ChatMessage =
  | { role: 'user'; text: string; ts: number }
  | {
      role: 'orchestrator';
      action: OrchestratorAction;
      text: string;
      reasoning: string;
      ts: number;
    }
  | { role: 'job'; jobId: string; goal: string; ts: number };

/**
 * Map a persisted `OrchestratorMessage` (DB shape) to the local
 * `ChatMessage` (UI shape). Orchestrator rows JSON-decode the
 * `content` column back into the typed outcome — a parse failure
 * surfaces as a degraded `direct_reply` bubble carrying the raw
 * content so the user sees *something* rather than a missing row.
 */
function persistedToChat(msg: OrchestratorMessage): ChatMessage {
  switch (msg.role) {
    case 'user':
      return { role: 'user', text: msg.content, ts: msg.createdAtMs };
    case 'orchestrator': {
      try {
        const outcome: OrchestratorOutcome = JSON.parse(msg.content);
        return {
          role: 'orchestrator',
          action: outcome.action,
          text: outcome.text,
          reasoning: outcome.reasoning,
          ts: msg.createdAtMs,
        };
      } catch {
        return {
          role: 'orchestrator',
          action: 'direct_reply',
          text: msg.content,
          reasoning: '',
          ts: msg.createdAtMs,
        };
      }
    }
    case 'job':
      return {
        role: 'job',
        jobId: msg.content,
        goal: msg.goal ?? '',
        ts: msg.createdAtMs,
      };
  }
}

export interface UseOrchestratorChat {
  /** Full displayed thread: `[...seed, ...local]`. */
  messages: ChatMessage[];
  input: string;
  setInput: (value: string) => void;
  /** A decide or run_job mutation is in flight. */
  submitting: boolean;
  /** The clear-history mutation is in flight. */
  clearing: boolean;
  /** Inline error banner text, or null. */
  error: string | null;
  /** Attach to the scrollable history container for auto-scroll. */
  historyRef: RefObject<HTMLDivElement>;
  handleSubmit: () => Promise<void>;
  handleClear: () => void;
}

export function useOrchestratorChat(workspaceId: string): UseOrchestratorChat {
  // `localMessages` carries new bubbles produced this session (typed →
  // user, decided → orchestrator, dispatched → job). The displayed
  // list is `[...seedMessages, ...localMessages]` where `seedMessages`
  // is the persisted thread from `useOrchestratorHistory`. Splitting
  // the two sources keeps the seed-vs-live boundary explicit and avoids
  // the "setState inside useEffect" anti-pattern (the seed is derived).
  //
  // `cleared` is a one-way local override: clicking "Clear chat" sets
  // it `true` so the seed is hidden until a remount. New local bubbles
  // still render (the user can keep chatting after a clear) but the
  // persisted seed is suppressed in favour of the empty-after-clear
  // thread.
  const [localMessages, setLocalMessages] = useState<ChatMessage[]>([]);
  const [cleared, setCleared] = useState(false);
  const [input, setInput] = useState('');
  const [error, setError] = useState<string | null>(null);
  const decide = useOrchestratorDecide();
  const runJob = useRunSwarmJob();
  const logJob = useLogOrchestratorJob();
  const clearHistory = useClearOrchestratorHistory();
  const history = useOrchestratorHistory(workspaceId);
  const qc = useQueryClient();
  const historyRef = useRef<HTMLDivElement>(null);

  // Persisted thread mapped to chat shape. `cleared` flips this to `[]`
  // after the Clear button fires; `useMemo` recomputes only on the
  // inputs that matter so an unrelated re-render doesn't churn the list.
  const seedMessages = useMemo<ChatMessage[]>(() => {
    if (cleared) return [];
    return history.data ? history.data.map(persistedToChat) : [];
  }, [history.data, cleared]);

  const messages = useMemo<ChatMessage[]>(
    () => [...seedMessages, ...localMessages],
    [seedMessages, localMessages],
  );

  // Auto-scroll history to bottom on new message / pending state change
  // so the latest bubble is always visible without manual scrolling.
  useEffect(() => {
    if (historyRef.current) {
      historyRef.current.scrollTop = historyRef.current.scrollHeight;
    }
  }, [messages, decide.isPending, runJob.isPending]);

  const submitting = decide.isPending || runJob.isPending;

  async function handleSubmit(): Promise<void> {
    const text = input.trim();
    if (!text || submitting) return;
    setError(null);
    setInput('');
    const userTs = Date.now();
    setLocalMessages((m) => [...m, { role: 'user', text, ts: userTs }]);
    try {
      const outcome = await decide.mutateAsync({ workspaceId, userMessage: text });
      setLocalMessages((m) => [
        ...m,
        {
          role: 'orchestrator',
          action: outcome.action,
          text: outcome.text,
          reasoning: outcome.reasoning,
          ts: Date.now(),
        },
      ]);
      if (outcome.action === 'dispatch') {
        const jobOutcome = await runJob.mutateAsync({
          workspaceId,
          goal: outcome.text,
        });
        setLocalMessages((m) => [
          ...m,
          {
            role: 'job',
            jobId: jobOutcome.jobId,
            goal: outcome.text,
            ts: Date.now(),
          },
        ]);
        // Persist the job row so the chat thread shows it on next mount.
        // Failure here is non-fatal — the in-memory bubble already
        // rendered; only the next mount misses it.
        try {
          await logJob.mutateAsync({
            workspaceId,
            jobId: jobOutcome.jobId,
            goal: outcome.text,
          });
        } catch {
          // Non-fatal — see WP §"Notes / risks".
        }
      }
      // No mid-session ['orchestrator-history'] invalidate — see the
      // module header for why (avoids duplicating every persisted turn).
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  function handleClear(): void {
    clearHistory.mutate(workspaceId, {
      onSuccess: () => {
        // Suppress the seed and drop any local bubbles so the displayed
        // list is empty until the next user turn.
        setCleared(true);
        setLocalMessages([]);
        setError(null);
        // Invalidate so a follow-up mount (or post-reload load) sees the
        // empty thread instead of stale cached data.
        qc.invalidateQueries({
          queryKey: ['orchestrator-history', workspaceId],
        });
      },
    });
  }

  return {
    messages,
    input,
    setInput,
    submitting,
    clearing: clearHistory.isPending,
    error,
    historyRef,
    handleSubmit,
    handleClear,
  };
}

// `OrchestratorChatPanel` — chat-shaped UI for the 9th Swarm
// agent (the Orchestrator). Replaces the W3-14 SwarmGoalForm:
// instead of a single goal textarea, the user converses with
// the Orchestrator persona; per message it decides to reply
// directly, ask a clarifying question, or refine the goal and
// dispatch a swarm job.
//
// W3-12k1 shipped the stateless brain. W3-12k3 shipped this UI
// with React-only state. W3-12k2 (this revision) layers SQLite
// persistence on top:
//
// - On mount: `useOrchestratorHistory(workspaceId)` reads the
//   persisted thread and seeds the local `messages` state.
// - On every successful decide / dispatch / clear: invalidate the
//   history query so subsequent mounts see the latest thread.
// - "Clear chat" button (top-right of the history area) calls
//   `useClearOrchestratorHistory` and resets local state.
//
// Submit flow:
//   1. Append user bubble to local state (immediate echo).
//   2. Call `useOrchestratorDecide` → outcome (backend persists user
//      row before invoke + orchestrator row after parse).
//   3. Append orchestrator bubble (action-tinted).
//   4. If `action === 'dispatch'`, chain `useRunSwarmJob` then
//      `useLogOrchestratorJob` to record the dispatched job; append
//      a job bubble that links into the right pane (SwarmJobDetail)
//      via `onSelectJob`.
//   5. Invalidate `['orchestrator-history', workspaceId]` so the
//      next mount picks up the persisted rows.
//   6. On any failure, render an inline error banner above the
//      input row; the user can retry.
import { useState, useRef, useEffect, useMemo } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import type { OrchestratorAction, OrchestratorMessage, OrchestratorOutcome } from '../lib/bindings';
import { useOrchestratorDecide } from '../hooks/useOrchestratorDecide';
import { useRunSwarmJob } from '../hooks/useRunSwarmJob';
import { useOrchestratorHistory } from '../hooks/useOrchestratorHistory';
import { useClearOrchestratorHistory } from '../hooks/useClearOrchestratorHistory';
import { useLogOrchestratorJob } from '../hooks/useLogOrchestratorJob';

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

interface Props {
  workspaceId: string;
  onSelectJob: (jobId: string) => void;
}

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

export function OrchestratorChatPanel({
  workspaceId,
  onSelectJob,
}: Props): JSX.Element {
  // `localMessages` carries new bubbles the user has produced this
  // session (typed → user, decided → orchestrator, dispatched →
  // job). The displayed list is `[...seedMessages, ...localMessages]`
  // where `seedMessages` is the persisted thread from
  // `useOrchestratorHistory`. Splitting the two sources keeps the
  // seed-vs-live boundary explicit and avoids the "setState inside
  // useEffect" anti-pattern (the seed is derived, not assigned).
  //
  // `cleared` is a one-way local override: clicking "Clear chat"
  // sets it `true` so the seed is hidden until a remount. New
  // local bubbles still render (the user can keep chatting after a
  // clear) but the persisted seed is suppressed in favour of the
  // empty-after-clear thread.
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

  // Persisted thread mapped to chat shape. `cleared` flips this to
  // `[]` after the Clear button fires; `useMemo` recomputes only on
  // the inputs that matter so an unrelated re-render doesn't churn
  // the displayed list. Subsequent invalidations of the history
  // query will refetch the seed — we suppress that with `cleared`
  // until a fresh mount, otherwise mid-session "Clear chat" would
  // briefly flicker the old thread back in if the cache hadn't
  // yet realized it was stale.
  const seedMessages = useMemo<ChatMessage[]>(() => {
    if (cleared) return [];
    return history.data ? history.data.map(persistedToChat) : [];
  }, [history.data, cleared]);

  const messages = useMemo<ChatMessage[]>(
    () => [...seedMessages, ...localMessages],
    [seedMessages, localMessages],
  );

  // Auto-scroll history to bottom on new message / pending state
  // change so the latest bubble is always visible without manual
  // scrolling.
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
        // Persist the job row so the chat thread shows it on next
        // mount. Failure here is non-fatal — the in-memory bubble
        // already rendered; only the next mount misses it.
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
      // We deliberately do NOT invalidate
      // `['orchestrator-history']` mid-session: the seed snapshot
      // taken at mount + the locally appended bubbles already cover
      // the displayed thread, and a refetch would duplicate every
      // turn (seed re-arrives WITH the just-persisted user +
      // orchestrator rows). The next mount re-runs the query
      // automatically via TanStack's mount-time fetch.
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  function handleClear(): void {
    clearHistory.mutate(workspaceId, {
      onSuccess: () => {
        // Suppress the seed and drop any local bubbles so the
        // displayed list is empty until the next user turn.
        setCleared(true);
        setLocalMessages([]);
        setError(null);
        // Invalidate so a follow-up mount (or post-reload load)
        // sees the empty thread instead of stale cached data.
        qc.invalidateQueries({
          queryKey: ['orchestrator-history', workspaceId],
        });
      },
    });
  }

  return (
    <div className="swarm-chat">
      <div className="swarm-chat-toolbar">
        <button
          type="button"
          className="btn ghost swarm-chat-clear-btn"
          onClick={handleClear}
          disabled={messages.length === 0 || clearHistory.isPending || submitting}
          aria-label="Clear chat"
        >
          Clear chat
        </button>
      </div>
      <div className="swarm-chat-history" ref={historyRef}>
        {messages.length === 0 && (
          <div className="swarm-chat-empty">
            <p>Chat with the Swarm Orchestrator.</p>
            <p>Ask questions or describe what you want to build.</p>
          </div>
        )}
        {messages.map((m, i) => (
          <ChatBubble key={i} msg={m} onSelectJob={onSelectJob} />
        ))}
        {submitting && (
          <div className="swarm-chat-msg orchestrator pending" aria-live="polite">
            <span className="thinking-dots" aria-label="Thinking">
              <span />
              <span />
              <span />
            </span>
          </div>
        )}
      </div>
      {error && (
        <div className="swarm-chat-error" role="alert">
          {error}
        </div>
      )}
      <div className="swarm-chat-input-row">
        <textarea
          className="swarm-chat-input"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              void handleSubmit();
            }
          }}
          disabled={submitting}
          placeholder="Type a message…"
          rows={2}
          aria-label="Chat with Orchestrator"
        />
        <button
          type="button"
          className="btn primary"
          disabled={submitting || !input.trim()}
          onClick={() => void handleSubmit()}
        >
          {submitting ? 'Sending…' : 'Send'}
        </button>
      </div>
    </div>
  );
}

function ChatBubble({
  msg,
  onSelectJob,
}: {
  msg: ChatMessage;
  onSelectJob: (jobId: string) => void;
}): JSX.Element {
  if (msg.role === 'user') {
    return <div className="swarm-chat-msg user">{msg.text}</div>;
  }
  if (msg.role === 'orchestrator') {
    return (
      <div
        className={`swarm-chat-msg orchestrator action-${msg.action}`}
        title={msg.reasoning}
      >
        {msg.text}
      </div>
    );
  }
  // role === 'job'
  const truncatedGoal =
    msg.goal.length > 80 ? `${msg.goal.slice(0, 80)}…` : msg.goal;
  return (
    <div className="swarm-chat-msg job">
      <span className="swarm-chat-job-prefix">Started job </span>
      <button
        type="button"
        className="link-button swarm-chat-job-link"
        onClick={() => onSelectJob(msg.jobId)}
      >
        {msg.jobId.slice(0, 8)}
      </button>
      <span className="swarm-chat-job-goal mute">{truncatedGoal}</span>
    </div>
  );
}

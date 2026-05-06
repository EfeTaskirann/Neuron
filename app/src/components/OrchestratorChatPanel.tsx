// `OrchestratorChatPanel` — chat-shaped UI for the 9th Swarm
// agent (the Orchestrator). Replaces the W3-14 SwarmGoalForm:
// instead of a single goal textarea, the user converses with
// the Orchestrator persona; per message it decides to reply
// directly, ask a clarifying question, or refine the goal and
// dispatch a swarm job.
//
// Stateless per W3-12k1 — each `swarm:orchestrator_decide` call
// is independent. Chat history is local component state only;
// W3-12k-2 will layer SQLite persistence on top so reload
// preserves the thread.
//
// Submit flow:
//   1. Append user bubble.
//   2. Call `useOrchestratorDecide` → outcome.
//   3. Append orchestrator bubble (action-tinted).
//   4. If `action === 'dispatch'`, chain `useRunSwarmJob` with
//      the refined `outcome.text` and append a job bubble that
//      links into the right pane (SwarmJobDetail) via
//      `onSelectJob`.
//   5. On any failure, render an inline error banner above the
//      input row; the user can retry.
import { useState, useRef, useEffect } from 'react';
import type { OrchestratorAction } from '../lib/bindings';
import { useOrchestratorDecide } from '../hooks/useOrchestratorDecide';
import { useRunSwarmJob } from '../hooks/useRunSwarmJob';

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

export function OrchestratorChatPanel({
  workspaceId,
  onSelectJob,
}: Props): JSX.Element {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [error, setError] = useState<string | null>(null);
  const decide = useOrchestratorDecide();
  const runJob = useRunSwarmJob();
  const historyRef = useRef<HTMLDivElement>(null);

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
    setMessages((m) => [...m, { role: 'user', text, ts: userTs }]);
    try {
      const outcome = await decide.mutateAsync({ workspaceId, userMessage: text });
      setMessages((m) => [
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
        setMessages((m) => [
          ...m,
          {
            role: 'job',
            jobId: jobOutcome.jobId,
            goal: outcome.text,
            ts: Date.now(),
          },
        ]);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <div className="swarm-chat">
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

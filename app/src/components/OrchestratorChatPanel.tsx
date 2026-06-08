// `OrchestratorChatPanel` — chat-shaped UI for the 9th Swarm agent
// (the Orchestrator). Replaces the W3-14 SwarmGoalForm: instead of a
// single goal textarea, the user converses with the Orchestrator
// persona; per message it decides to reply directly, ask a clarifying
// question, or refine the goal and dispatch a swarm job.
//
// Presentation only — the chat state machine (persisted-history seed,
// the decide → dispatch → log submit chain, clear) lives in
// `useOrchestratorChat` (BACKLOG T2-02 sunum/efekt ayrımı). See that
// hook for the WP-W3-12k1/k2/k3 history.
import { useOrchestratorChat } from '../hooks/useOrchestratorChat';
import type { ChatMessage } from '../hooks/useOrchestratorChat';

// Re-exported for backwards compatibility with importers that pulled
// the chat-message shape from this module before the T2-02 split.
export type { ChatMessage };

interface Props {
  workspaceId: string;
  onSelectJob: (jobId: string) => void;
}

export function OrchestratorChatPanel({
  workspaceId,
  onSelectJob,
}: Props): JSX.Element {
  const {
    messages,
    input,
    setInput,
    submitting,
    clearing,
    error,
    historyRef,
    handleSubmit,
    handleClear,
  } = useOrchestratorChat(workspaceId);

  return (
    <div className="swarm-chat">
      <div className="swarm-chat-toolbar">
        <button
          type="button"
          className="btn ghost swarm-chat-clear-btn"
          onClick={handleClear}
          disabled={messages.length === 0 || clearing || submitting}
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

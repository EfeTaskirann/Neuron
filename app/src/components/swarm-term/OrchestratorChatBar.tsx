import { useState, type FormEvent } from 'react';

interface OrchestratorChatBarProps {
  /** Deliver a trimmed, non-empty message to the orchestrator pane.
   *  Parent owns the pane id + write mutation. */
  onSend: (message: string) => void;
  /** Autonomous-mode flag + setter. Lifted to the parent route so the
   *  localStorage/body-attr sync stays live whenever the route is
   *  mounted, not only while this (session-gated) bar is shown. */
  autonomous: boolean;
  setAutonomous: (next: boolean) => void;
}

// Chat bar for the `@orchestrator` pane: autonomous-mode toggle + text
// input. Owns its own draft state so the parent route doesn't reconcile
// on every keystroke.
export function OrchestratorChatBar({
  onSend,
  autonomous,
  setAutonomous,
}: OrchestratorChatBarProps): JSX.Element {
  const [chatInput, setChatInput] = useState('');

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const msg = chatInput.trim();
    if (!msg) return;
    onSend(msg);
    setChatInput('');
  };

  return (
    <form
      className={`swarm-term-chat${autonomous ? ' swarm-term-chat--auto' : ''}`}
      onSubmit={submit}
    >
      <label
        className="swarm-term-auto-toggle"
        title="When ON the swarm runs end-to-end without per-step approval prompts."
      >
        <input
          type="checkbox"
          checked={autonomous}
          onChange={(e) => setAutonomous(e.target.checked)}
          aria-label="Run autonomously — suppress approval prompts"
        />
        <span aria-hidden="true" className="swarm-term-auto-toggle-track">
          <span className="swarm-term-auto-toggle-thumb" />
        </span>
        <span className="swarm-term-auto-toggle-label">Run autonomously</span>
      </label>
      {autonomous && (
        <span
          className="swarm-term-auto-chip"
          role="status"
          aria-label="Autonomous mode active"
        >
          AUTO
        </span>
      )}
      <input
        type="text"
        placeholder="Talk to @orchestrator — type your task and hit Enter"
        value={chatInput}
        onChange={(e) => setChatInput(e.target.value)}
        aria-label="Message to orchestrator"
      />
      <button type="submit" className="btn primary sm" disabled={!chatInput.trim()}>
        Send
      </button>
    </form>
  );
}

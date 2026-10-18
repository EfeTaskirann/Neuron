import { useState, type FormEvent } from 'react';
import type { SwarmTermPermissionMode } from '../../lib/bindings';

interface OrchestratorChatBarProps {
  /** Deliver a trimmed, non-empty message to the orchestrator pane.
   *  Parent owns the pane id + write mutation. */
  onSend: (message: string) => void;
  /** Mode the session's panes were spawned with (from the backend). */
  permissionMode: SwarmTermPermissionMode;
  /** Set while @orchestrator cannot take a task yet: why. A message
   *  sent before its kickoff answer could collide with the kickoff. */
  disabledReason: string | null;
}

/** What the restricted mode (`dontAsk` + per-role allowlist) allows —
 *  mirrors `swarm_term::permissions::allowed_tools`. */
export const RESTRICTED_PERMISSIONS_DETAIL =
  'Agents never stop to ask for approval. Each role works from an allowlist: ' +
  'builders can edit files inside this project and run build, test, lint and ' +
  'git-inspection commands; reviewers and the integration tester can run those ' +
  'commands; the orchestrator, coordinator, scout and planner only read the ' +
  'project and send messages. Anything else is denied without asking, and nobody ' +
  "writes .git, .claude, editor config or another agent's messages. Your own " +
  'Claude settings, hooks and MCP servers are not loaded into the panes; this ' +
  "project's committed settings and CLAUDE.md are.";

export const BYPASS_PERMISSIONS_DETAIL =
  'NEURON_TERM_PERMISSIONS=bypass is set: every agent runs with ' +
  'bypassPermissions, i.e. unrestricted shell and file access as your user ' +
  'account.';

const PERMISSION_DETAIL_ID = 'swarm-term-perm-detail';
const CHAT_WAIT_ID = 'swarm-term-chat-wait';

// Chat bar for the `@orchestrator` pane: the session's permission mode +
// text input. Owns its own draft state so the parent route doesn't
// reconcile on every keystroke.
export function OrchestratorChatBar({
  onSend,
  permissionMode,
  disabledReason,
}: OrchestratorChatBarProps): JSX.Element {
  const [chatInput, setChatInput] = useState('');
  const bypass = permissionMode === 'bypass';
  const permissionDetail = bypass
    ? BYPASS_PERMISSIONS_DETAIL
    : RESTRICTED_PERMISSIONS_DETAIL;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const msg = chatInput.trim();
    if (!msg || disabledReason) return;
    onSend(msg);
    setChatInput('');
  };

  return (
    <form className="swarm-term-chat" onSubmit={submit}>
      {/* The chip is focusable so keyboard users can open the
          explanation too; it shows on hover and focus, and screen
          readers get it once, as the chip's description. */}
      <span className="swarm-term-perm">
        <span
          className={`swarm-term-perm-chip${bypass ? ' swarm-term-perm-chip--bypass' : ''}`}
          tabIndex={0}
          aria-describedby={PERMISSION_DETAIL_ID}
        >
          {bypass ? 'Permissions bypassed · unrestricted' : 'No approval prompts · role allowlists'}
        </span>
        <span id={PERMISSION_DETAIL_ID} role="tooltip" className="swarm-term-perm-tip">
          {permissionDetail}
        </span>
      </span>
      <input
        type="text"
        placeholder={disabledReason ?? 'Talk to @orchestrator — type your task and hit Enter'}
        value={chatInput}
        onChange={(e) => setChatInput(e.target.value)}
        aria-label="Message to orchestrator"
        disabled={disabledReason != null}
        aria-describedby={disabledReason ? CHAT_WAIT_ID : undefined}
      />
      {disabledReason && (
        <span id={CHAT_WAIT_ID} className="sr-only">
          {disabledReason}
        </span>
      )}
      <button
        type="submit"
        className="btn primary sm"
        disabled={!chatInput.trim() || disabledReason != null}
      >
        Send
      </button>
    </form>
  );
}

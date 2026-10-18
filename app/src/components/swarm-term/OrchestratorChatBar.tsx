import { useState, type FormEvent } from 'react';
import type { SwarmTermPermissionMode } from '../../lib/bindings';

interface OrchestratorChatBarProps {
  /** Deliver a trimmed, non-empty message to the orchestrator pane.
   *  Parent owns the pane id + write mutation. */
  onSend: (message: string) => void;
  /** Mode the session's panes were spawned with (from the backend). */
  permissionMode: SwarmTermPermissionMode;
}

/** What the restricted mode (`dontAsk` + per-role allowlist) allows —
 *  mirrors `swarm_term::permissions::allowed_tools`. */
export const RESTRICTED_PERMISSIONS_DETAIL =
  'Agents never stop to ask for approval. Each role works from an allowlist: ' +
  'builders can edit files inside this project and run build, test, lint and ' +
  'git-inspection commands; reviewers and the integration tester can run those ' +
  'commands; the orchestrator, coordinator, scout and planner only read the ' +
  'project and send messages. Anything else is denied without asking, unless ' +
  "your own or this project's Claude settings already allow it.";

export const BYPASS_PERMISSIONS_DETAIL =
  'NEURON_TERM_PERMISSIONS=bypass is set: every agent runs with ' +
  'bypassPermissions, i.e. unrestricted shell and file access as your user ' +
  'account.';

// Chat bar for the `@orchestrator` pane: the session's permission mode +
// text input. Owns its own draft state so the parent route doesn't
// reconcile on every keystroke.
export function OrchestratorChatBar({
  onSend,
  permissionMode,
}: OrchestratorChatBarProps): JSX.Element {
  const [chatInput, setChatInput] = useState('');
  const bypass = permissionMode === 'bypass';
  const permissionDetail = bypass
    ? BYPASS_PERMISSIONS_DETAIL
    : RESTRICTED_PERMISSIONS_DETAIL;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const msg = chatInput.trim();
    if (!msg) return;
    onSend(msg);
    setChatInput('');
  };

  return (
    <form className="swarm-term-chat" onSubmit={submit}>
      <span
        className={`swarm-term-perm-chip${bypass ? ' swarm-term-perm-chip--bypass' : ''}`}
        title={permissionDetail}
      >
        {bypass ? 'Permissions bypassed · unrestricted' : 'No approval prompts · role allowlists'}
        <span className="sr-only">. {permissionDetail}</span>
      </span>
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

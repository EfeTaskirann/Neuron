// Shared display metadata for the Terminal route's pane components.

export interface AgentInfo {
  name: string;
  accent: string;
  icon: 'claude' | 'openai' | 'gemini' | 'shell';
}

// Display metadata indexed by Pane.agent (claude/codex/gemini/
// shell). Kept UI-side because the prototype's `data.agents`
// lookup table was UI-only and never going to be a backend
// concern.
const AGENT_META: Record<string, AgentInfo> = {
  'claude-code': { name: 'Claude', accent: 'claude', icon: 'claude' },
  codex: { name: 'Codex', accent: 'openai', icon: 'openai' },
  gemini: { name: 'Gemini', accent: 'gemini', icon: 'gemini' },
  shell: { name: 'Shell', accent: 'shell', icon: 'shell' },
};

export function metaFor(agent: string): AgentInfo {
  return AGENT_META[agent] ?? { name: agent, accent: 'shell', icon: 'shell' };
}

// Panes in a terminal state — kill alone won't free the row, only
// `terminal:purge_closed` will. UI uses this set to skip the live
// `panes:{id}:line` subscription (dead PTY emits nothing) and to
// disable the per-tab close button in favour of the bulk cleanup.
export const TERMINAL_STATUSES = new Set(['closed', 'error', 'success']);

export const STATUS_LABEL: Record<string, string> = {
  idle: 'idle',
  running: 'running',
  awaiting_approval: 'awaiting',
  success: 'done',
  error: 'error',
  starting: 'starting',
  closed: 'closed',
};

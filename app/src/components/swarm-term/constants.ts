import type { AgentLifecycle } from '../../hooks/useRoutingEvents';

// Fixed 3×3 slot order for the agent terminal grid. Both the grid
// render and the arrow-key navigation index into this array, so the
// visual order and the focus math always agree.
export const SLOT_ORDER = [
  'orchestrator',
  'coordinator',
  'scout',
  'planner',
  'backend-builder',
  'frontend-builder',
  'backend-reviewer',
  'frontend-reviewer',
  'integration-tester',
];

export const LIFECYCLE_LABEL: Record<AgentLifecycle, string> = {
  idle: 'idle',
  assigned: 'assigned',
  building: 'building',
  review: 'review',
  done: 'done',
};

// session_id from the backend is `swarm-term-<26-char ULID>`. A ULID's
// first 10 chars are a 48-bit ms-since-epoch timestamp in Crockford
// Base32 — we decode that to derive launch time without a separate
// backend field.
const ULID_BASE32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const ULID_RE = /^swarm-term-([0-9A-HJKMNP-TV-Z]{26})$/i;

export function parseSwarmTermStartMs(sessionId: string): number | null {
  const match = ULID_RE.exec(sessionId);
  if (!match) return null;
  const tsChars = match[1].slice(0, 10).toUpperCase();
  let n = 0;
  for (const ch of tsChars) {
    const v = ULID_BASE32.indexOf(ch);
    if (v < 0) return null;
    n = n * 32 + v;
  }
  return n;
}

export function formatMmSs(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

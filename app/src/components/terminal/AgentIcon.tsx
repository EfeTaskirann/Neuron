import type { AgentInfo } from './agentMeta';

export function AgentIcon({
  kind,
  accent,
}: {
  kind: AgentInfo['icon'];
  accent: string;
}): JSX.Element {
  const c = `var(--agent-${accent})`;
  if (kind === 'claude')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="9" fill="none" stroke={c} strokeWidth="1.6" />
        <path
          d="M8 9 L12 15 L16 9"
          stroke={c}
          strokeWidth="1.6"
          fill="none"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
    );
  if (kind === 'openai')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <path
          d="M12 3 L20 8 V16 L12 21 L4 16 V8 Z"
          fill="none"
          stroke={c}
          strokeWidth="1.6"
          strokeLinejoin="round"
        />
      </svg>
    );
  if (kind === 'gemini')
    return (
      <svg width="14" height="14" viewBox="0 0 24 24">
        <path d="M12 2 L14 10 L22 12 L14 14 L12 22 L10 14 L2 12 L10 10 Z" fill={c} />
      </svg>
    );
  return (
    <svg width="14" height="14" viewBox="0 0 24 24">
      <path
        d="M5 9 L9 12 L5 15 M11 16 L17 16"
        stroke={c}
        strokeWidth="1.6"
        fill="none"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

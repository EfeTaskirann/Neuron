import type { AgentLifecycle } from '../../hooks/useRoutingEvents';
import { LIFECYCLE_LABEL } from './constants';

interface LifecyclePillProps {
  phase: AgentLifecycle;
  agentId: string;
}

export function LifecyclePill({ phase, agentId }: LifecyclePillProps): JSX.Element {
  return (
    <span
      className={`swarm-term-pane-phase swarm-term-pane-phase--${phase}`}
      role="status"
      aria-label={`Lifecycle phase for ${agentId}: ${LIFECYCLE_LABEL[phase]}`}
    >
      {LIFECYCLE_LABEL[phase]}
    </span>
  );
}

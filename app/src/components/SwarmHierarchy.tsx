import type { AgentLifecycle } from '../hooks/useRoutingEvents';

// Hierarchy visualization grouping. Mirrors the four tiers documented
// in `src-tauri/src/swarm_term/hierarchy.rs` ALLOWED graph and the
// brief's acceptance criterion (a). Ordering inside a tier is the
// same as the visit order Orchestrator/Coordinator use when
// dispatching — top → bottom in the read-out matches the natural
// flow Orchestration → Research → Build → Review.
const TIERS: ReadonlyArray<{
  id: string;
  label: string;
  agents: readonly string[];
}> = [
  { id: 'orchestration', label: 'Orchestration', agents: ['orchestrator', 'coordinator'] },
  { id: 'research', label: 'Research', agents: ['scout', 'planner'] },
  { id: 'build', label: 'Build', agents: ['backend-builder', 'frontend-builder'] },
  {
    id: 'review',
    label: 'Review',
    agents: ['backend-reviewer', 'frontend-reviewer', 'integration-tester'],
  },
];

interface HierarchyDiagramProps {
  lifecycle: Record<string, AgentLifecycle>;
  activeSource: string | null;
  activeTarget: string | null;
  personasById: Map<string, { id: string; role: string; description: string }>;
  panesByAgent: Map<string, string>;
}

/**
 * Compact tier-grouped hierarchy strip. Renders one chip per agent
 * grouped by tier, glows the most-recent `(src, dst)` chip pair when
 * `activeSource`/`activeTarget` are set, and folds the per-agent
 * lifecycle phase into each chip's `data-phase` so CSS can subtly
 * recolour the chip without us having to ship a phase-specific class
 * for every state.
 */
export function HierarchyDiagram({
  lifecycle,
  activeSource,
  activeTarget,
  personasById,
  panesByAgent,
}: HierarchyDiagramProps): JSX.Element {
  const liveLabel =
    activeSource && activeTarget
      ? `Live route: ${activeSource} to ${activeTarget}`
      : 'No active route';
  return (
    <nav
      className="swarm-term-hierarchy"
      aria-label="Swarm hierarchy and live route"
    >
      <span className="sr-only" aria-live="polite">
        {liveLabel}
      </span>
      {TIERS.map((tier) => (
        <div key={tier.id} className={`swarm-term-hier-tier tier-${tier.id}`}>
          <span className="swarm-term-hier-label">{tier.label}</span>
          <ul className="swarm-term-hier-row" role="list">
            {tier.agents.map((agentId) => {
              const persona = personasById.get(agentId);
              const phase: AgentLifecycle = lifecycle[agentId] ?? 'idle';
              const hasPane = panesByAgent.has(agentId);
              const isSource = activeSource === agentId;
              const isTarget = activeTarget === agentId;
              const classes = [
                'swarm-term-hier-chip',
                isSource ? 'is-source' : '',
                isTarget ? 'is-target' : '',
                hasPane ? '' : 'is-offline',
              ]
                .filter(Boolean)
                .join(' ');
              return (
                <li key={agentId}>
                  <span
                    className={classes}
                    data-phase={phase}
                    title={
                      persona
                        ? `${persona.role}: ${persona.description}`
                        : agentId
                    }
                  >
                    <span className="swarm-term-hier-chip-id">@{agentId}</span>
                  </span>
                </li>
              );
            })}
          </ul>
        </div>
      ))}
    </nav>
  );
}

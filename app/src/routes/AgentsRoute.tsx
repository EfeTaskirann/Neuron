// Ports `Neuron Design/app/routes.jsx::AgentsRoute`. DOM and class
// names unchanged per ADR-0005; only the data source moves from
// `window.NeuronData.agents` to `useAgents()`.
import { NIcon, NodeGlyph } from '../components/icons';
import { useAgents } from '../hooks/useAgents';
import type { Agent } from '../lib/bindings';

export function AgentsRoute(): JSX.Element {
  const { data: agents = [], isLoading, isError, error } = useAgents();
  if (isLoading) {
    return <div className="route route-agents route-loading">Loading agents…</div>;
  }
  if (isError) {
    // ErrorBoundary upstream catches throw paths; isError is a soft
    // surface, e.g. when retry is exhausted. Throw so the boundary
    // shows the recoverable card.
    throw error instanceof Error ? error : new Error(String(error));
  }
  return (
    <div className="route route-agents">
      <div className="route-grid">
        {agents.map((a: Agent) => (
          <div key={a.id} className="agent-card">
            <div className="agent-card-head">
              <div className="agent-avatar">
                <NodeGlyph kind="llm" size={22} />
              </div>
              <div>
                <div className="agent-name">{a.name}</div>
                <div className="agent-model">
                  {a.model} · temp {a.temp}
                </div>
              </div>
              <div className="agent-spacer" />
              <span className="pill st-ok">ready</span>
            </div>
            <div className="agent-role">{a.role}</div>
            <div className="agent-foot">
              <button className="btn ghost sm">
                <NIcon name="copy" size={12} />
                <span>Duplicate</span>
              </button>
              <button className="btn ghost sm">
                <span>Open</span>
                <NIcon name="chevronR" size={12} />
              </button>
            </div>
          </div>
        ))}
        <div className="agent-card add">
          <NIcon name="plus" size={22} />
          <span>New agent</span>
        </div>
      </div>
    </div>
  );
}

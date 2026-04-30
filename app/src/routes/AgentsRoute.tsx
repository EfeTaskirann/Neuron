// Ports `Neuron Design/app/routes.jsx::AgentsRoute`. DOM and class
// names unchanged per ADR-0005; data source moves from
// `window.NeuronData.agents` to `useAgents()`. Phase E adds the
// "+ New agent" inline form (agentsCreate mutation) and a
// per-card delete button (agentsDelete).
import { useState, type FormEvent } from 'react';
import { NIcon, NodeGlyph } from '../components/icons';
import { useAgents } from '../hooks/useAgents';
import { useAgentCreate, useAgentDelete } from '../hooks/mutations';
import type { Agent } from '../lib/bindings';

export function AgentsRoute(): JSX.Element {
  const { data: agents = [], isLoading, isError, error } = useAgents();
  const [creating, setCreating] = useState(false);
  if (isLoading) {
    return <div className="route route-agents route-loading">Loading agents…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  return (
    <div className="route route-agents">
      <div className="route-grid">
        {agents.map((a: Agent) => (
          <AgentCard key={a.id} agent={a} />
        ))}
        {creating ? (
          <NewAgentForm onClose={() => setCreating(false)} />
        ) : (
          <button
            className="agent-card add"
            type="button"
            onClick={() => setCreating(true)}
          >
            <NIcon name="plus" size={22} />
            <span>New agent</span>
          </button>
        )}
      </div>
    </div>
  );
}

function AgentCard({ agent }: { agent: Agent }): JSX.Element {
  const del = useAgentDelete();
  return (
    <div className="agent-card">
      <div className="agent-card-head">
        <div className="agent-avatar">
          <NodeGlyph kind="llm" size={22} />
        </div>
        <div>
          <div className="agent-name">{agent.name}</div>
          <div className="agent-model">
            {agent.model} · temp {agent.temp}
          </div>
        </div>
        <div className="agent-spacer" />
        <span className="pill st-ok">ready</span>
      </div>
      <div className="agent-role">{agent.role}</div>
      <div className="agent-foot">
        <button className="btn ghost sm">
          <NIcon name="copy" size={12} />
          <span>Duplicate</span>
        </button>
        <button
          className="btn ghost sm"
          disabled={del.isPending}
          onClick={() => {
            if (confirm(`Delete agent "${agent.name}"?`)) {
              del.mutate(agent.id);
            }
          }}
          title="Delete agent"
        >
          <NIcon name="trash" size={12} />
          <span>{del.isPending ? 'Deleting…' : 'Delete'}</span>
        </button>
      </div>
    </div>
  );
}

function NewAgentForm({ onClose }: { onClose: () => void }): JSX.Element {
  const create = useAgentCreate();
  const [name, setName] = useState('');
  const [model, setModel] = useState('gpt-4o');
  const [temp, setTemp] = useState(0.2);
  const [role, setRole] = useState('');

  const handleSubmit = (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    create.mutate(
      { name: name.trim(), model: model.trim(), temp, role: role.trim() },
      { onSuccess: () => onClose() },
    );
  };

  return (
    <form className="agent-card agent-card-new" onSubmit={handleSubmit}>
      <div className="agent-card-head">
        <strong>New agent</strong>
        <div className="agent-spacer" />
        <button
          type="button"
          className="icon-btn sm"
          onClick={onClose}
          title="Cancel"
        >
          <NIcon name="close" size={12} />
        </button>
      </div>
      <label className="agent-form-row">
        <span>Name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="Planner"
          required
        />
      </label>
      <label className="agent-form-row">
        <span>Model</span>
        <input value={model} onChange={(e) => setModel(e.target.value)} required />
      </label>
      <label className="agent-form-row">
        <span>Temp</span>
        <input
          type="number"
          step="0.1"
          min="0"
          max="2"
          value={temp}
          onChange={(e) => setTemp(Number(e.target.value))}
        />
      </label>
      <label className="agent-form-row">
        <span>Role</span>
        <input
          value={role}
          onChange={(e) => setRole(e.target.value)}
          placeholder="Plans the day"
        />
      </label>
      <div className="agent-foot">
        <button type="button" className="btn ghost sm" onClick={onClose}>
          Cancel
        </button>
        <button
          type="submit"
          className="btn primary sm"
          disabled={create.isPending || !name.trim()}
        >
          {create.isPending ? 'Creating…' : 'Create'}
        </button>
      </div>
    </form>
  );
}

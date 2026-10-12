// Models settings pane. Read-only catalog of the models available to
// agents — per-agent model selection happens in the Agents tab; provider
// API keys live under Keys.
interface ModelInfo {
  id: string;
  provider: string;
  note: string;
}

const MODEL_CATALOG: ModelInfo[] = [
  { id: 'claude-opus-4', provider: 'Anthropic', note: 'Most capable; drives the swarm agents.' },
  { id: 'claude-sonnet-4', provider: 'Anthropic', note: 'Balanced cost and quality.' },
  { id: 'claude-haiku-4', provider: 'Anthropic', note: 'Fastest and cheapest.' },
  { id: 'gpt-4o', provider: 'OpenAI', note: 'Default model for new agents.' },
  { id: 'gemini-2.5-pro', provider: 'Google', note: 'Long-context, multimodal.' },
];

export function ModelsPane(): JSX.Element {
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Models
      </h2>
      <p className="text-muted">
        Models available to agents. Pick a per-agent model in the Agents tab;
        provider API keys live under Keys.
      </p>
      <div className="set-card">
        {MODEL_CATALOG.map((m) => (
          <div className="set-row" key={m.id}>
            <div>
              <div className="set-row-title">{m.id}</div>
              <div className="set-row-sub">{m.note}</div>
            </div>
            <span className="text-muted">{m.provider}</span>
          </div>
        ))}
      </div>
    </>
  );
}

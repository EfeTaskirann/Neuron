// Models settings pane: the Terminal Swarm's cost controls — which team
// runs, each agent's model, and how many messages the agents may send
// one another in a session. Kept in Neuron's settings (see
// `swarm_term::team`); a change applies when the swarm next starts.
import { useState } from 'react';
import { SLOT_ORDER } from '../swarm-term/constants';
import {
  useSaveSwarmTermTeam,
  useSwarmTermPersonas,
  useSwarmTermTeam,
} from '../../hooks/useTerminalSwarmSession';
import type { SwarmTermTeam } from '../../lib/bindings';
import { describeError } from '../../lib/unwrap';

/** claude's model aliases (`team::MODEL_ALIASES`); '' = claude's default. */
const MODELS: { value: string; label: string }[] = [
  { value: '', label: 'Varsayılan' },
  { value: 'opus', label: 'Opus' },
  { value: 'sonnet', label: 'Sonnet' },
  { value: 'haiku', label: 'Haiku' },
];

/** Mirrors `team::LITE_TEAM`. */
const LITE_TEAM = ['orchestrator', 'coordinator', 'backend-builder', 'backend-reviewer'];

const MAX_BUDGET = 100_000;

function ModelChoice({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string | null;
  onChange: (model: string) => void;
}): JSX.Element {
  return (
    <div className="seg" role="radiogroup" aria-label={label}>
      {MODELS.map((m) => (
        <button
          key={m.value}
          role="radio"
          aria-checked={value === m.value}
          className={value === m.value ? 'active' : ''}
          onClick={() => onChange(m.value)}
        >
          {m.label}
        </button>
      ))}
    </div>
  );
}

export function ModelsPane(): JSX.Element {
  const { data: team, error } = useSwarmTermTeam();
  const { data: personas = [] } = useSwarmTermPersonas();
  const save = useSaveSwarmTermTeam();
  const [budgetDraft, setBudgetDraft] = useState<string | null>(null);

  const header = (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Models
      </h2>
      <p className="text-muted">
        Terminal Swarm’ın hangi takımla ve hangi modellerle çalışacağı, bir oturumda ajanlar
        arası kaç mesaja izin verildiği. Değişiklikler swarm bir sonraki başlatıldığında
        geçerli olur.
      </p>
    </>
  );
  if (!team) {
    return (
      <>
        {header}
        {error != null && (
          <p className="text-muted" role="alert">
            {describeError(error)}
          </p>
        )}
      </>
    );
  }

  const update = (next: SwarmTermTeam) => save.mutate(next);
  const setModel = (agents: string[], model: string) => {
    const models = { ...team.models };
    for (const agent of agents) {
      if (model) models[agent] = model;
      else delete models[agent];
    }
    update({ ...team, models });
  };
  const roleOf = (agent: string) => personas.find((p) => p.id === agent)?.role ?? agent;
  const allSame = SLOT_ORDER.every((a) => (team.models[a] ?? '') === (team.models[SLOT_ORDER[0]!] ?? ''));

  const budgetText = budgetDraft ?? String(team.messageBudget);
  const budgetValue = Number(budgetText);
  const budgetValid =
    /^\d+$/.test(budgetText.trim()) && Number.isInteger(budgetValue) && budgetValue <= MAX_BUDGET;
  const commitBudget = () => {
    if (budgetValid && budgetValue !== team.messageBudget) {
      update({ ...team, messageBudget: budgetValue });
    }
    if (budgetValid) setBudgetDraft(null);
  };

  return (
    <>
      {header}
      {save.error != null && (
        <p className="text-muted" role="alert">
          {describeError(save.error)}
        </p>
      )}

      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Takım</div>
            <div className="set-row-sub">
              Lite takım yalnız orchestrator, coordinator, backend-builder ve backend-reviewer’ı
              açar: dört pane, çok daha az kullanım. Araştırma ve plan orchestrator’da, bütün kod
              builder’da, bütün review reviewer’da.
            </div>
          </div>
          <div className="seg" role="radiogroup" aria-label="Takım">
            {[
              { lite: false, label: 'Tam (9)' },
              { lite: true, label: 'Lite (4)' },
            ].map((t) => (
              <button
                key={t.label}
                role="radio"
                aria-checked={team.lite === t.lite}
                className={team.lite === t.lite ? 'active' : ''}
                onClick={() => update({ ...team, lite: t.lite })}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>

        <div className="set-row">
          <div>
            <div className="set-row-title">Mesaj bütçesi</div>
            <div className="set-row-sub">
              Bir oturumda ajanlar arası en çok bu kadar mesaj teslim edilir; dolunca swarm durur
              ve devam etmek için sana sorar. 0: sınırsız.
            </div>
          </div>
          <input
            type="text"
            inputMode="numeric"
            aria-label="Mesaj bütçesi"
            aria-invalid={!budgetValid}
            value={budgetText}
            onChange={(e) => setBudgetDraft(e.target.value)}
            onBlur={commitBudget}
            onKeyDown={(e) => {
              if (e.key === 'Enter') commitBudget();
            }}
            style={{ width: 96 }}
          />
        </div>
      </div>

      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Bütün ajanlar</div>
            <div className="set-row-sub">
              Varsayılan: Claude’un kendi ayarındaki model. Haiku en ucuzu, Opus en yeteneklisi.
            </div>
          </div>
          <ModelChoice
            label="Bütün ajanların modeli"
            value={allSame ? (team.models[SLOT_ORDER[0]!] ?? '') : null}
            onChange={(model) => setModel(SLOT_ORDER, model)}
          />
        </div>
        {SLOT_ORDER.map((agent) => {
          const absent = team.lite && !LITE_TEAM.includes(agent);
          return (
            <div className="set-row" key={agent} style={absent ? { opacity: 0.55 } : undefined}>
              <div>
                <div className="set-row-title">{roleOf(agent)}</div>
                <div className="set-row-sub">
                  @{agent}
                  {absent ? ' — Lite takımda yok' : ''}
                </div>
              </div>
              <ModelChoice
                label={`${agent} modeli`}
                value={team.models[agent] ?? ''}
                onChange={(model) => setModel([agent], model)}
              />
            </div>
          );
        })}
      </div>
    </>
  );
}

// Ports `Neuron Design/app/routes.jsx::SettingsRoute`. Appearance
// pane is the only one with backing state today — its controls
// flow through `useAppearance`, which persists to localStorage and
// drives the `<html>` element (theme class, density/motion data
// attributes, accent CSS vars). The other sections still render
// the placeholder card per the prototype until their panes land.
import { useState } from 'react';
import { NIcon, type IconName } from '../components/icons';
import {
  ACCENT_SWATCHES,
  useAppearance,
  type Density,
  type Motion,
  type Theme,
} from '../hooks/useAppearance';
import { useSecretHas, useSecretSet, useSecretDelete } from '../hooks/useSecrets';
import { useMe } from '../hooks/useMe';
import { useRuns } from '../hooks/useRuns';

interface SettingsSection {
  id: string;
  label: string;
  icon: IconName;
}

const SECTIONS: SettingsSection[] = [
  { id: 'account', label: 'Account', icon: 'bot' },
  { id: 'appearance', label: 'Appearance', icon: 'sun' },
  { id: 'workflows', label: 'Workflows', icon: 'workflow' },
  { id: 'agents', label: 'Agents', icon: 'sparkles' },
  { id: 'models', label: 'Models', icon: 'zap' },
  { id: 'mcp', label: 'MCP', icon: 'store' },
  { id: 'keys', label: 'Keys', icon: 'plug' },
  { id: 'data', label: 'Data', icon: 'layers' },
];

export function SettingsRoute(): JSX.Element {
  const [active, setActive] = useState('appearance');
  const activeSection = SECTIONS.find((s) => s.id === active) ?? SECTIONS[0];
  return (
    <div className="route route-settings">
      <nav className="settings-nav">
        {SECTIONS.map((s) => (
          <button
            key={s.id}
            className={`set-item${active === s.id ? ' active' : ''}`}
            onClick={() => setActive(s.id)}
          >
            <NIcon name={s.icon} size={15} />
            <span>{s.label}</span>
          </button>
        ))}
      </nav>
      <div className="settings-pane">
        {active === 'appearance' ? (
          <AppearancePane />
        ) : active === 'keys' ? (
          <KeysPane />
        ) : active === 'account' ? (
          <AccountPane />
        ) : active === 'data' ? (
          <DataPane />
        ) : (
          <div className="set-empty">
            <h2 className="text-h2" style={{ marginTop: 0 }}>
              {activeSection.label}
            </h2>
            <p className="text-muted">Settings for this section.</p>
          </div>
        )}
      </div>
    </div>
  );
}

const THEMES: { value: Theme; label: string }[] = [
  { value: 'light', label: 'Light' },
  { value: 'dark', label: 'Dark' },
  { value: 'system', label: 'System' },
];

const DENSITIES: { value: Density; label: string }[] = [
  { value: 'comfortable', label: 'Comfortable' },
  { value: 'compact', label: 'Compact' },
];

const MOTIONS: { value: Motion; label: string }[] = [
  { value: 'full', label: 'Full' },
  { value: 'reduced', label: 'Reduced' },
  { value: 'off', label: 'Off' },
];

function AppearancePane(): JSX.Element {
  const { theme, accent, density, motion, setTheme, setAccent, setDensity, setMotion } =
    useAppearance();

  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Appearance
      </h2>
      <p className="text-muted">Colors, density, and motion. Changes apply instantly.</p>

      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Theme</div>
            <div className="set-row-sub">Match the OS or pick one.</div>
          </div>
          <div className="seg" role="radiogroup" aria-label="Theme">
            {THEMES.map((t) => (
              <button
                key={t.value}
                role="radio"
                aria-checked={theme === t.value}
                className={theme === t.value ? 'active' : ''}
                onClick={() => setTheme(t.value)}
              >
                {t.label}
              </button>
            ))}
          </div>
        </div>

        <div className="set-row">
          <div>
            <div className="set-row-title">Accent</div>
            <div className="set-row-sub">
              Used on selection, focus, and Synapse Violet surfaces.
            </div>
          </div>
          <div className="swatches" role="radiogroup" aria-label="Accent color">
            {ACCENT_SWATCHES.map((c) => (
              <button
                key={c}
                role="radio"
                aria-label={c}
                aria-checked={accent === c}
                className={`sw${accent === c ? ' active' : ''}`}
                style={{ background: c }}
                onClick={() => setAccent(c)}
              />
            ))}
          </div>
        </div>

        <div className="set-row">
          <div>
            <div className="set-row-title">Density</div>
            <div className="set-row-sub">Comfortable spacing or tighter rows.</div>
          </div>
          <div className="seg" role="radiogroup" aria-label="Density">
            {DENSITIES.map((d) => (
              <button
                key={d.value}
                role="radio"
                aria-checked={density === d.value}
                className={density === d.value ? 'active' : ''}
                onClick={() => setDensity(d.value)}
              >
                {d.label}
              </button>
            ))}
          </div>
        </div>

        <div className="set-row">
          <div>
            <div className="set-row-title">Motion</div>
            <div className="set-row-sub">Edge dataflow, node pulse, glow shimmer.</div>
          </div>
          <div className="seg" role="radiogroup" aria-label="Motion">
            {MOTIONS.map((m) => (
              <button
                key={m.value}
                role="radio"
                aria-checked={motion === m.value}
                className={motion === m.value ? 'active' : ''}
                onClick={() => setMotion(m.value)}
              >
                {m.label}
              </button>
            ))}
          </div>
        </div>
      </div>
    </>
  );
}

interface KeySlot {
  id: string;
  label: string;
  placeholder: string;
}

// API key slots. `id` is the keychain key the backend stores under —
// the bare provider name, mirroring the secrets:* tests + the
// `AppError::NoApiKey` consumers (mcp:install, runs:create).
const KEY_SLOTS: KeySlot[] = [
  { id: 'anthropic', label: 'Anthropic (Claude)', placeholder: 'sk-ant-…' },
  { id: 'openai', label: 'OpenAI', placeholder: 'sk-…' },
  { id: 'gemini', label: 'Google Gemini', placeholder: 'AIza…' },
];

function KeysPane(): JSX.Element {
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Keys
      </h2>
      <p className="text-muted">
        API keys live in your OS keychain — never in plaintext or synced.
        Neuron can tell whether a key is set but never reads the value back.
      </p>
      <div className="set-card">
        {KEY_SLOTS.map((slot) => (
          <KeyRow key={slot.id} slot={slot} />
        ))}
      </div>
    </>
  );
}

function KeyRow({ slot }: { slot: KeySlot }): JSX.Element {
  const has = useSecretHas(slot.id);
  const setSecret = useSecretSet();
  const del = useSecretDelete();
  const [value, setValue] = useState('');
  const configured = has.data === true;

  const save = () => {
    const v = value.trim();
    if (!v) return;
    setSecret.mutate(
      { key: slot.id, value: v },
      { onSuccess: () => setValue('') },
    );
  };

  return (
    <div className="set-row">
      <div>
        <div className="set-row-title">{slot.label}</div>
        <div className="set-row-sub">
          {has.isLoading ? 'Checking…' : configured ? 'Configured ✓' : 'Not set'}
        </div>
      </div>
      <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
        <input
          type="password"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder={slot.placeholder}
          aria-label={`${slot.label} API key`}
        />
        <button
          type="button"
          className="btn primary sm"
          onClick={save}
          disabled={!value.trim() || setSecret.isPending}
        >
          {setSecret.isPending ? 'Saving…' : 'Save'}
        </button>
        {configured && (
          <button
            type="button"
            className="btn ghost sm"
            onClick={() => del.mutate(slot.id)}
            disabled={del.isPending}
            title={`Forget ${slot.label} key`}
          >
            {del.isPending ? 'Forgetting…' : 'Forget'}
          </button>
        )}
      </div>
    </div>
  );
}

function AccountPane(): JSX.Element {
  const me = useMe();
  const user = me.data?.user;
  const ws = me.data?.workspace;
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Account
      </h2>
      <p className="text-muted">
        Neuron runs locally as a single-user desktop app — there is no cloud
        account or sign-in.
      </p>
      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">User</div>
            <div className="set-row-sub">
              {me.isLoading
                ? 'Loading…'
                : `${user?.name ?? '—'} · ${user?.initials ?? '··'}`}
            </div>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Workspace</div>
            <div className="set-row-sub">
              {me.isLoading
                ? 'Loading…'
                : `${ws?.name ?? '—'} · ${ws?.count ?? 0} workflows`}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}

function DataPane(): JSX.Element {
  const runs = useRuns();
  const list = runs.data ?? [];
  const totalCost = list.reduce((sum, r) => sum + r.cost, 0);
  const exportRuns = (): void => {
    const blob = new Blob([JSON.stringify(list, null, 2)], {
      type: 'application/json',
    });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `neuron-runs-${list.length}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Data
      </h2>
      <p className="text-muted">
        Your runs, agents, and settings live in a local SQLite database on
        this machine — nothing is synced.
      </p>
      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">Run history</div>
            <div className="set-row-sub">
              {runs.isLoading
                ? 'Loading…'
                : `${list.length} runs · $${totalCost.toFixed(4)} total`}
            </div>
          </div>
          <button
            type="button"
            className="btn ghost sm"
            onClick={exportRuns}
            disabled={runs.isLoading || list.length === 0}
          >
            Export JSON
          </button>
        </div>
      </div>
    </>
  );
}

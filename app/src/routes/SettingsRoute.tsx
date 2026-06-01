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

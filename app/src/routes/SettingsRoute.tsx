// Ports `Neuron Design/app/routes.jsx::SettingsRoute`. No data
// dependencies — Appearance pane is the only one wired, the rest
// surface a placeholder card per the prototype. Theme/density/
// motion controls are presentational stubs (Week 3 wires real
// persistence via the Tweaks pipeline).
import { useState } from 'react';
import { NIcon, type IconName } from '../components/icons';

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

function AppearancePane(): JSX.Element {
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
          <div className="seg">
            <button>Light</button>
            <button className="active">Dark</button>
            <button>System</button>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Accent</div>
            <div className="set-row-sub">
              Used on selection, focus, and Synapse Violet surfaces.
            </div>
          </div>
          <div className="swatches">
            {['#a874d6', '#7aa6f0', '#e0a85b', '#7ad6c8', '#d678a6'].map((c, i) => (
              <button
                key={c}
                className={`sw${i === 0 ? ' active' : ''}`}
                style={{ background: c }}
              />
            ))}
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Density</div>
            <div className="set-row-sub">Comfortable spacing or tighter rows.</div>
          </div>
          <div className="seg">
            <button className="active">Comfortable</button>
            <button>Compact</button>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Motion</div>
            <div className="set-row-sub">Edge dataflow, node pulse, glow shimmer.</div>
          </div>
          <div className="seg">
            <button className="active">Full</button>
            <button>Reduced</button>
            <button>Off</button>
          </div>
        </div>
      </div>
    </>
  );
}

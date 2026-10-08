// Ports `Neuron Design/app/routes.jsx::SettingsRoute`. This file is the
// nav + pane-switch shell; each section's body lives in its own
// component under `components/settings/` (split from the original
// monolith — behaviour is preserved). Appearance is the only pane with
// backing state today (via `useAppearance`); Workflows/Agents/MCP are
// signposts to their dedicated tabs, and the rest render placeholders
// per the prototype until their panes land.
import { useState } from 'react';
import { NIcon, type IconName } from '../components/icons';
import { AppearancePane } from '../components/settings/AppearancePane';
import { KeysPane } from '../components/settings/KeysPane';
import { AccountPane } from '../components/settings/AccountPane';
import { DataPane } from '../components/settings/DataPane';
import { ModelsPane } from '../components/settings/ModelsPane';
import { LinkPane } from '../components/settings/LinkPane';

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
        ) : active === 'models' ? (
          <ModelsPane />
        ) : active === 'workflows' ? (
          <LinkPane
            label="Workflows"
            body="Workflows are built visually on the canvas. Open the Workflow tab to create, edit, or run them."
          />
        ) : active === 'agents' ? (
          <LinkPane
            label="Agents"
            body="Agents — name, model, temperature, and role — are configured in the Agents tab."
          />
        ) : active === 'mcp' ? (
          <LinkPane
            label="MCP"
            body="Browse, install, and uninstall MCP servers from the MCP tab. Per-server API keys live under Keys."
          />
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

// Keys settings pane. Drives the `secrets:*` keychain commands —
// API keys live in the OS keychain, never in plaintext or synced.
// Neuron can probe whether a key is set but never reads the value back.
import { useState } from 'react';
import { useSecretHas, useSecretSet, useSecretDelete } from '../../hooks/useSecrets';

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

export function KeysPane(): JSX.Element {
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
          {has.isLoading ? (
            'Checking…'
          ) : has.isError ? (
            // Probe failure ≠ "not set" — saying "Not set" here would
            // hide the Forget affordance for a key that may exist.
            <>
              Status unreadable{' '}
              <button
                type="button"
                className="btn ghost sm"
                onClick={() => void has.refetch()}
              >
                Retry
              </button>
            </>
          ) : configured ? (
            'Configured ✓'
          ) : (
            'Not set'
          )}
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

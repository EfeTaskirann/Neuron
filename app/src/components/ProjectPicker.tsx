import { useState } from 'react';
import { open } from '@tauri-apps/plugin-dialog';
import { NIcon } from './icons';

interface Props {
  value: string | null;
  onChange: (path: string | null) => void;
  disabled?: boolean;
}

export function ProjectPicker({ value, onChange, disabled }: Props): JSX.Element {
  const [pending, setPending] = useState(false);
  const pick = async () => {
    setPending(true);
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === 'string') {
        onChange(selected);
      }
    } finally {
      setPending(false);
    }
  };
  return (
    <div className="swarm-term-picker">
      <button
        type="button"
        className="btn ghost sm"
        onClick={pick}
        disabled={disabled || pending}
      >
        <NIcon name="server" size={14} />
        <span>{pending ? 'Picking…' : value ? 'Change project' : 'Pick project'}</span>
      </button>
      <div className="swarm-term-picker-path" title={value ?? ''}>
        {value ?? <em>No project selected</em>}
      </div>
    </div>
  );
}

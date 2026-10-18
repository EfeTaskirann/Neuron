import { useState } from 'react';
import { ask } from '@tauri-apps/plugin-dialog';
import {
  useKeepSwarmTermChanges,
  useRevertSwarmTermChanges,
  useSwarmTermChangeDiff,
} from '../../hooks/useSwarmTermChanges';
import type { ChangeKind, SwarmTermChanges } from '../../lib/bindings';
import { describeError } from '../../lib/unwrap';

interface ChangesDrawerProps {
  projectDir: string;
  /** The change list (owned by the route: the toolbar counts it too). */
  changes: SwarmTermChanges | undefined;
  /** When the list was last read (ms) — refreshes an open diff. */
  updatedAt: number;
  fetching: boolean;
  error: unknown;
  onRefresh: () => void;
}

const KIND_LETTER: Record<ChangeKind, string> = {
  added: 'A',
  modified: 'M',
  deleted: 'D',
  type_changed: 'T',
};

const KIND_LABEL: Record<ChangeKind, string> = {
  added: 'eklendi',
  modified: 'değişti',
  deleted: 'silindi',
  type_changed: 'türü değişti',
};

function formatTime(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toLocaleString('tr-TR', {
    day: '2-digit',
    month: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function diffLineClass(line: string): string {
  if (line.startsWith('@@')) return 'swarm-term-diff-hunk';
  if (line.startsWith('+') && !line.startsWith('+++')) return 'swarm-term-diff-add';
  if (line.startsWith('-') && !line.startsWith('---')) return 'swarm-term-diff-del';
  return '';
}

function confirm(message: string, okLabel: string): Promise<boolean> {
  return ask(message, { title: 'Değişiklikler', kind: 'warning', okLabel, cancelLabel: 'Vazgeç' });
}

// What the agents changed since the checkpoint the session took at its
// start: each file with its diff, and per file or all at once the way
// back (revert) or forward (keep: the checkpoint moves to now).
export function ChangesDrawer({
  projectDir,
  changes,
  updatedAt,
  fetching,
  error,
  onRefresh,
}: ChangesDrawerProps): JSX.Element {
  const [selected, setSelected] = useState<string | null>(null);
  const files = changes?.files ?? [];
  const shown = selected != null && files.some((f) => f.path === selected) ? selected : null;
  const diff = useSwarmTermChangeDiff(projectDir, shown, updatedAt);
  const revertMut = useRevertSwarmTermChanges(projectDir);
  const keepMut = useKeepSwarmTermChanges(projectDir);
  const busy = revertMut.isPending || keepMut.isPending;
  const actionError = revertMut.error ?? keepMut.error;

  const revert = async (paths: string[] | null) => {
    const what =
      paths == null
        ? `${files.length} dosya checkpoint'teki haline dönsün mü? Checkpoint'ten sonraki bütün değişiklikler (seninkiler de) kaybolur.`
        : `"${paths[0]}" checkpoint'teki haline dönsün mü? Bu dosyadaki değişiklikler kaybolur.`;
    if (await confirm(what, 'Geri al')) revertMut.mutate(paths);
  };

  const keepAll = async () => {
    if (
      await confirm(
        'Değişiklikler kalsın ve checkpoint şimdiye taşınsın mı? Bu değişiklikler artık buradan geri alınamaz.',
        'Tut',
      )
    ) {
      keepMut.mutate();
    }
  };

  return (
    <div className="swarm-term-changes">
      <div className="swarm-term-changes-head">
        <h2>Değişiklikler</h2>
        {changes?.checkpointAt != null && (
          <span className="swarm-term-changes-since">
            checkpoint: {formatTime(changes.checkpointAt)}
          </span>
        )}
        <button type="button" className="btn ghost sm" onClick={onRefresh} disabled={fetching}>
          {fetching ? 'Okunuyor…' : 'Yenile'}
        </button>
      </div>

      {changes?.note && (
        <p className="swarm-term-note" role="status">
          {changes.note}
        </p>
      )}
      {(error != null || actionError != null) && (
        <p className="swarm-term-error" role="alert">
          {describeError(actionError ?? error)}
        </p>
      )}

      {files.length === 0 ? (
        changes?.checkpointAt != null && (
          <p className="swarm-term-changes-empty">Checkpoint'ten beri değişen dosya yok.</p>
        )
      ) : (
        <>
          <div className="swarm-term-changes-actions">
            <span>
              {files.length}
              {changes?.truncated ? '+' : ''} dosya
            </span>
            <button type="button" className="btn ghost sm" onClick={keepAll} disabled={busy}>
              Tümünü tut
            </button>
            <button type="button" className="btn ghost sm" onClick={() => revert(null)} disabled={busy}>
              Tümünü geri al
            </button>
          </div>
          <ul className="swarm-term-changes-list">
            {files.map((f) => (
              <li key={f.path} className={f.path === shown ? 'selected' : undefined}>
                <button
                  type="button"
                  className="swarm-term-change-file"
                  aria-pressed={f.path === shown}
                  onClick={() => setSelected(f.path === shown ? null : f.path)}
                  title={`${f.path} — ${KIND_LABEL[f.kind]}`}
                >
                  <span className={`swarm-term-change-kind swarm-term-change-kind--${f.kind}`}>
                    {KIND_LETTER[f.kind]}
                  </span>
                  <span className="swarm-term-change-path">{f.path}</span>
                  <span className="swarm-term-change-counts">
                    {f.additions == null ? 'ikili' : `+${f.additions} −${f.deletions ?? 0}`}
                  </span>
                </button>
                <button
                  type="button"
                  className="btn ghost sm"
                  aria-label={`${f.path}: geri al`}
                  onClick={() => revert([f.path])}
                  disabled={busy}
                >
                  Geri al
                </button>
              </li>
            ))}
          </ul>
        </>
      )}

      {shown != null && (
        <div className="swarm-term-diff-box" aria-label={`${shown} diff`}>
          {diff.isLoading && <p className="swarm-term-changes-empty">Diff okunuyor…</p>}
          {diff.error != null && (
            <p className="swarm-term-error" role="alert">
              {describeError(diff.error)}
            </p>
          )}
          {diff.data != null && (
            <pre className="swarm-term-diff">
              {diff.data.split('\n').map((line, i) => (
                <span key={i} className={diffLineClass(line)}>
                  {line}
                  {'\n'}
                </span>
              ))}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}

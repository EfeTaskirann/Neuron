import type { ClaudeUpdateResult } from '../../lib/bindings';
import type { ClaudeUpdateProgress } from '../../hooks/useClaudeUpdateProgress';

interface ClaudeUpdateCornerProps {
  /** True while a session is live — Claude can't be updated then. */
  sessionActive: boolean;
  isPending: boolean;
  progress: ClaudeUpdateProgress;
  result: ClaudeUpdateResult | undefined;
  error: unknown;
  onUpdate: () => void;
}

// Bottom-corner "Update Claude" affordance: the button plus the live
// progress line and the terminal ✓/✗ status. Disabled while a session
// is running so an in-flight swarm isn't torn out from under itself.
export function ClaudeUpdateCorner({
  sessionActive,
  isPending,
  progress,
  result,
  error,
  onUpdate,
}: ClaudeUpdateCornerProps): JSX.Element {
  return (
    <div className="swarm-term-update-corner">
      <button
        type="button"
        className="btn ghost sm"
        disabled={sessionActive || isPending}
        onClick={onUpdate}
        title={
          sessionActive
            ? "Önce session'ı kapat (Stop)"
            : isPending
              ? 'Güncelleme sürüyor…'
              : undefined
        }
      >
        {isPending ? 'Updating…' : 'Update Claude'}
      </button>
      {isPending && progress.lastLine && (
        <div
          className="swarm-term-update-progress"
          title={progress.lines.join('\n')}
        >
          {progress.lastLine}
        </div>
      )}
      {result && !isPending && (
        <span
          className={`swarm-term-update-status${
            result.exitCode === 0
              ? ' swarm-term-update-status--ok'
              : ' swarm-term-update-status--err'
          }`}
          title={
            result.exitCode === 0
              ? result.stdoutTail.slice(-400) || 'updated'
              : result.stderrTail.slice(-400) || `exit ${result.exitCode}`
          }
        >
          {result.exitCode === 0 ? '✓ updated' : '✗ failed'}
        </span>
      )}
      {error != null && (
        <span
          className="swarm-term-update-status swarm-term-update-status--err"
          title={error instanceof Error ? error.message : String(error)}
        >
          ✗ {error instanceof Error ? error.message.slice(0, 80) : 'failed'}
        </span>
      )}
    </div>
  );
}

import { getCurrentWindow, UserAttentionType } from '@tauri-apps/api/window';
import { showToast } from './toast';

// Notifications for the terminal swarm. The bridge reports every closed
// task to the orchestrator as a synthesised `TASK_DONE <id> — …` /
// `TASK_FAILED <id> — …` message; the routing store hands each one here.
// The usage limit and an agent's exit come from the session's events. A
// toast says it in the app, and the taskbar flashes when the window is
// in the background.

export interface TaskSignal {
  kind: 'done' | 'failed';
  taskId: string;
  /** What follows the id (the verdict / the reason), if anything. */
  note: string;
}

const SIGNAL_RE = /^\s*(TASK_DONE|TASK_FAILED)\s+(\S+)\s*([\s\S]*)$/;

/** Pure: the task signal at the start of a fanout body, if it is one.
 *  The note is whatever follows the id (the backend writes it with a
 *  dash — `— verdict` — or without — `(düzeltme: …)`); a leading dash is
 *  dropped. */
export function taskSignal(body: string): TaskSignal | null {
  const m = SIGNAL_RE.exec(body);
  if (!m) return null;
  const note = (m[3] ?? '').replace(/^[\s—–-]+/, '').trim();
  return {
    kind: m[1] === 'TASK_DONE' ? 'done' : 'failed',
    taskId: m[2]!,
    note,
  };
}

const NOTE_MAX = 160;

/** Toast (and, in the background, a taskbar flash) for a task signal. */
export function notifyTaskSignal(body: string): void {
  const signal = taskSignal(body);
  if (!signal) return;
  const note =
    signal.note.length > NOTE_MAX ? `${signal.note.slice(0, NOTE_MAX - 1)}…` : signal.note;
  showToast(
    signal.kind === 'done'
      ? { variant: 'success', body: `Görev tamamlandı: ${signal.taskId}`, durationMs: 8000 }
      : {
          variant: 'error',
          body: `Görev başarısız: ${signal.taskId}${note ? ` — ${note}` : ''}`,
          durationMs: 12000,
        },
  );
  requestAttention();
}

/** A reset time (unix seconds) on the user's clock: "HH:MM" when it is
 *  today, "DD.MM HH:MM" otherwise — a weekly limit can reset days away. */
export function formatResetTime(resetsAt: number): string {
  const when = new Date(resetsAt * 1000);
  const today = when.toDateString() === new Date().toDateString();
  return when.toLocaleString(
    'tr-TR',
    today
      ? { hour: '2-digit', minute: '2-digit' }
      : { day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit' },
  );
}

/** Toast (and taskbar flash) when the usage limit holds the swarm. */
export function notifyUsageLimit(resetsAt: number | null): void {
  showToast({
    variant: 'error',
    body:
      'Claude kullanım limitine ulaşıldı; swarm duraklatıldı' +
      (resetsAt != null ? ` (sıfırlanma: ${formatResetTime(resetsAt)})` : '') +
      '.',
    durationMs: 12000,
  });
  requestAttention();
}

/** Toast (and taskbar flash) when the message budget holds the swarm. */
export function notifyBudgetReached(): void {
  showToast({
    variant: 'error',
    body: 'Mesaj bütçesi doldu; swarm durdu. Sürdürmek için bütçeyi artır.',
    durationMs: 12000,
  });
  requestAttention();
}

/** Toast (and taskbar flash) when an agent's claude process ended. */
export function notifyPaneExited(agentId: string): void {
  showToast({
    variant: 'error',
    body: `@${agentId} kapandı: pane başlığındaki ↻ onu aynı konuşmayla yeniden başlatır.`,
    durationMs: 10000,
  });
  requestAttention();
}

/** Toast (and taskbar flash) when an agent found an instruction in the
 *  project's text (a README, a comment, a rule file) and reported it
 *  instead of following it. */
export function notifyInjection(agentId: string, detail: string | null): void {
  showToast({
    variant: 'error',
    body:
      `@${agentId} proje metninde kendisine yönelik bir talimat gördü ve uygulamadı` +
      (detail ? `: ${detail}` : '.'),
    durationMs: 15000,
  });
  requestAttention();
}

/** Flash the taskbar while the window is in the background. */
function requestAttention(): void {
  if (typeof document !== 'undefined' && !document.hasFocus()) {
    try {
      void getCurrentWindow()
        .requestUserAttention(UserAttentionType.Informational)
        .catch(() => {});
    } catch {
      // No Tauri window (tests, a plain browser): the toast is enough.
    }
  }
}

import { describe, expect, it, vi, beforeEach } from 'vitest';

const requestUserAttention = vi.fn(() => Promise.resolve());
vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({ requestUserAttention }),
  UserAttentionType: { Critical: 1, Informational: 2 },
}));

import { notifyTaskSignal, taskSignal } from './swarmNotify';
import { dismissToast, subscribeToasts, type Toast } from './toast';

function currentToasts(): readonly Toast[] {
  let seen: readonly Toast[] = [];
  subscribeToasts((t) => {
    seen = t;
  })();
  return seen;
}

beforeEach(() => {
  for (const t of currentToasts()) dismissToast(t.id);
  requestUserAttention.mockClear();
});

describe('taskSignal', () => {
  it('reads the bridge fanouts for closed tasks', () => {
    expect(taskSignal('TASK_DONE fix-1 — Verdict: approved')).toEqual({
      kind: 'done',
      taskId: 'fix-1',
      note: 'Verdict: approved',
    });
    expect(taskSignal('TASK_FAILED p2 — 3. ret: düzeltme limiti aşıldı')).toEqual({
      kind: 'failed',
      taskId: 'p2',
      note: '3. ret: düzeltme limiti aşıldı',
    });
    expect(taskSignal('TASK_DONE p0-smoke')).toEqual({ kind: 'done', taskId: 'p0-smoke', note: '' });
  });

  it('ignores every other fanout', () => {
    expect(taskSignal('review fix-1 — README eklendi')).toBeNull();
    expect(taskSignal('retest p1 — düzeltildi')).toBeNull();
    expect(taskSignal('the TASK_DONE p1 arrived')).toBeNull();
  });
});

describe('notifyTaskSignal', () => {
  it('toasts a finished task, and a failed one with its reason', () => {
    notifyTaskSignal('TASK_DONE fix-1 — Verdict: approved');
    notifyTaskSignal('TASK_FAILED p2 — @backend-builder hata bildirdi: derleme kırık');
    const toasts = currentToasts();
    expect(toasts.map((t) => [t.variant, t.body])).toEqual([
      ['success', 'Görev tamamlandı: fix-1'],
      ['error', 'Görev başarısız: p2 — @backend-builder hata bildirdi: derleme kırık'],
    ]);
  });

  it('flashes the taskbar only while the window is in the background', () => {
    const focus = vi.spyOn(document, 'hasFocus');
    focus.mockReturnValue(true);
    notifyTaskSignal('TASK_DONE a1');
    expect(requestUserAttention).not.toHaveBeenCalled();
    focus.mockReturnValue(false);
    notifyTaskSignal('TASK_DONE a2');
    expect(requestUserAttention).toHaveBeenCalledTimes(1);
  });

  it('does nothing for a message that is not a task signal', () => {
    notifyTaskSignal('review fix-1');
    expect(currentToasts()).toHaveLength(0);
  });
});

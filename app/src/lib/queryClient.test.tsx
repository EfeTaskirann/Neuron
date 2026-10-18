import { describe, expect, it, beforeEach } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { ToastHost } from '../components/ToastHost';
import { queryClient } from './queryClient';
import { dismissToast, subscribeToasts, type Toast } from './toast';
import { AppErrorClient } from './unwrap';

function currentToasts(): readonly Toast[] {
  let seen: readonly Toast[] = [];
  subscribeToasts((t) => {
    seen = t;
  })();
  return seen;
}

async function failWith(err: unknown, meta?: { errorsShownInline?: boolean }): Promise<void> {
  const mutation = queryClient.getMutationCache().build(queryClient, {
    mutationFn: () => Promise.reject(err),
    meta,
  });
  await mutation.execute(undefined).catch(() => {});
}

beforeEach(() => {
  for (const t of currentToasts()) dismissToast(t.id);
});

describe('failed actions are never silent', () => {
  it('a mutation nobody reports gets an error toast with the backend detail', async () => {
    await failWith(new AppErrorClient('internal', 'git add: index.lock exists'));
    const [toast] = currentToasts();
    expect(toast?.variant).toBe('error');
    expect(toast?.body).toBe('Beklenmedik bir iç hata oluştu.');
    expect(toast?.detail).toBe('git add: index.lock exists');
  });

  it('a mutation whose caller shows its errors inline gets none', async () => {
    await failWith(new AppErrorClient('internal', 'x'), { errorsShownInline: true });
    expect(currentToasts()).toHaveLength(0);
  });

  it('the toast keeps the detail behind a disclosure', async () => {
    render(<ToastHost />);
    await act(async () => {
      await failWith(new AppErrorClient('mcp_server_spawn_failed', 'npx: command not found'));
    });
    expect(screen.getByText('MCP sunucusu başlatılamadı.')).toBeInTheDocument();
    const summary = screen.getByText('Ayrıntı');
    fireEvent.click(summary);
    expect(screen.getByText('npx: command not found')).toBeInTheDocument();
  });
});

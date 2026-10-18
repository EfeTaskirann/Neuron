import { describe, expect, it, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

vi.mock('../../lib/bindings', () => ({
  commands: {
    swarmTermChangeDiff: vi.fn(),
    swarmTermRevertChanges: vi.fn(),
    swarmTermKeepChanges: vi.fn(),
  },
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  ask: vi.fn(),
}));

import { ChangesDrawer } from './ChangesDrawer';
import type { SwarmTermChanges } from '../../lib/bindings';

const CHANGES: SwarmTermChanges = {
  checkpointAt: 1_790_000_000,
  files: [
    { path: 'src/orders.rs', kind: 'modified', additions: 1, deletions: 1 },
    { path: 'assets/logo.png', kind: 'added', additions: null, deletions: null },
  ],
  truncated: false,
  note: null,
};

function renderDrawer(changes: SwarmTermChanges = CHANGES): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <ChangesDrawer
        projectDir="C:\proj"
        changes={changes}
        updatedAt={1}
        fetching={false}
        error={null}
        onRefresh={() => {}}
      />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe('ChangesDrawer', () => {
  it('lists each changed file with its kind and line counts', () => {
    renderDrawer();
    expect(screen.getByText('src/orders.rs')).toBeInTheDocument();
    expect(screen.getByText('+1 −1')).toBeInTheDocument();
    // A binary file has no line counts.
    expect(screen.getByText('ikili')).toBeInTheDocument();
    expect(screen.getByText(/2\s*dosya/)).toBeInTheDocument();
  });

  it("shows a file's diff with the added and removed lines marked", async () => {
    const { commands } = await import('../../lib/bindings');
    vi.mocked(commands.swarmTermChangeDiff).mockResolvedValue({
      status: 'ok',
      data: '@@ -1 +1 @@\n-qty > 10\n+qty >= 10\n',
    });
    renderDrawer();
    fireEvent.click(screen.getByRole('button', { name: /^M src\/orders\.rs/ }));
    expect(await screen.findByText('+qty >= 10')).toHaveClass('swarm-term-diff-add');
    expect(screen.getByText('-qty > 10')).toHaveClass('swarm-term-diff-del');
    expect(screen.getByText('@@ -1 +1 @@')).toHaveClass('swarm-term-diff-hunk');
    expect(commands.swarmTermChangeDiff).toHaveBeenCalledWith('C:\\proj', 'src/orders.rs');
  });

  it('reverts a file only once the user confirms', async () => {
    const { commands } = await import('../../lib/bindings');
    const { ask } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(ask).mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    vi.mocked(commands.swarmTermRevertChanges).mockResolvedValue({
      status: 'ok',
      data: { ...CHANGES, files: [CHANGES.files[1]!] },
    });
    renderDrawer();
    const revert = screen.getByRole('button', { name: 'src/orders.rs: geri al' });
    fireEvent.click(revert);
    await waitFor(() => expect(ask).toHaveBeenCalledTimes(1));
    expect(commands.swarmTermRevertChanges).not.toHaveBeenCalled();
    fireEvent.click(revert);
    await waitFor(() =>
      expect(commands.swarmTermRevertChanges).toHaveBeenCalledWith('C:\\proj', ['src/orders.rs']),
    );
  });

  it('keeps everything, or reverts everything, after a confirmation', async () => {
    const { commands } = await import('../../lib/bindings');
    const { ask } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(ask).mockResolvedValue(true);
    const none = { ...CHANGES, files: [] };
    vi.mocked(commands.swarmTermKeepChanges).mockResolvedValue({ status: 'ok', data: none });
    vi.mocked(commands.swarmTermRevertChanges).mockResolvedValue({ status: 'ok', data: none });
    renderDrawer();
    fireEvent.click(screen.getByRole('button', { name: 'Tümünü tut' }));
    await waitFor(() => expect(commands.swarmTermKeepChanges).toHaveBeenCalledWith('C:\\proj'));
    fireEvent.click(screen.getByRole('button', { name: 'Tümünü geri al' }));
    await waitFor(() =>
      expect(commands.swarmTermRevertChanges).toHaveBeenCalledWith('C:\\proj', null),
    );
  });

  it('says what the snapshots leave out, and when nothing changed', () => {
    renderDrawer({
      checkpointAt: 1_790_000_000,
      files: [],
      truncated: false,
      note: "Projede 6000 izlenmeyen dosya var: checkpoint yalnız git'in izlediği dosyaları kapsıyor.",
    });
    expect(screen.getByRole('status')).toHaveTextContent('6000 izlenmeyen dosya');
    expect(screen.getByText("Checkpoint'ten beri değişen dosya yok.")).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Tümünü geri al' })).not.toBeInTheDocument();
  });
});

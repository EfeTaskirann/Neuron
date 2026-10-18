import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mirror `SwarmRoute.test.tsx`: stub the command surface the route reaches
// (server catalog + install/uninstall mutations), drive the real
// `useServers`/`useMcpInstall`/`useMcpUninstall` hooks through `unwrap`.
vi.mock('../lib/bindings', () => ({
  commands: {
    mcpList: vi.fn(),
    mcpInstall: vi.fn(),
    mcpUninstall: vi.fn(),
  },
}));

vi.mock('@tauri-apps/plugin-dialog', () => ({
  ask: vi.fn(),
}));

import { MCPRoute } from './MCPRoute';
import type { Server } from '../lib/bindings';

function renderRoute(): { qc: QueryClient } {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<MCPRoute />, { wrapper: Wrapper });
  return { qc };
}

const FS_SERVER: Server = {
  id: 'filesystem',
  name: 'Filesystem',
  by: 'Model Context Protocol',
  desc: 'Read, write and search files in its own sandbox folder',
  installed: false,
};

const GIT_SERVER: Server = {
  id: 'srv-git',
  name: 'Git',
  by: 'community',
  desc: 'Repo operations',
  installed: true,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.mcpList).mockResolvedValue({
    status: 'ok',
    data: [FS_SERVER, GIT_SERVER],
  });
  vi.mocked(commands.mcpInstall).mockResolvedValue({ status: 'ok', data: FS_SERVER });
  vi.mocked(commands.mcpUninstall).mockResolvedValue({ status: 'ok', data: GIT_SERVER });
});

/** The "installed" filter chip — an installed server's uninstall button
 *  is also named "installed" (its pill), so pick the chip by class. */
function installedChip(): HTMLElement {
  const chip = screen
    .getAllByRole('button', { name: 'installed' })
    .find((b) => b.classList.contains('chip'));
  if (!chip) throw new Error('no installed chip');
  return chip;
}

describe('MCPRoute', () => {
  it('shows the loading state before the servers query resolves', () => {
    renderRoute();
    expect(screen.getByText(/loading servers/i)).toBeInTheDocument();
  });

  it('lists each server once, with an honest intro and no invented stats', async () => {
    renderRoute();
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/search 2 servers/i)).toBeInTheDocument(),
    );
    expect(screen.getByText('Filesystem')).toBeInTheDocument();
    expect(screen.getByText('Git')).toBeInTheDocument();
    expect(screen.getByText(/swarm agents don't use these servers yet/i)).toBeInTheDocument();
    expect(screen.queryByText('Featured')).not.toBeInTheDocument();
    expect(screen.queryByText(/★/)).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'official' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'community' })).not.toBeInTheDocument();
  });

  it('renders the empty catalog without throwing', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.mcpList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/search 0 servers/i)).toBeInTheDocument(),
    );
    expect(screen.getByText(/no servers in the catalog/i)).toBeInTheDocument();
  });

  it('filters the catalog by the search box', async () => {
    renderRoute();
    await waitFor(() => expect(screen.getByText('Git')).toBeInTheDocument());
    fireEvent.change(screen.getByPlaceholderText(/search 2 servers/i), {
      target: { value: 'git' },
    });
    expect(screen.getByText('Git')).toBeInTheDocument();
    expect(screen.queryByText('Filesystem')).not.toBeInTheDocument();
  });

  it('uninstalls a server only after the user confirms', async () => {
    const { commands } = await import('../lib/bindings');
    const { ask } = await import('@tauri-apps/plugin-dialog');
    vi.mocked(commands.mcpUninstall).mockResolvedValue({ status: 'ok', data: FS_SERVER });
    vi.mocked(ask).mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    renderRoute();
    await waitFor(() => expect(installedChip()).toBeInTheDocument());
    const uninstall = screen.getByTitle('Uninstall server');
    fireEvent.click(uninstall);
    await waitFor(() => expect(ask).toHaveBeenCalledTimes(1));
    expect(commands.mcpUninstall).not.toHaveBeenCalled();
    fireEvent.click(uninstall);
    await waitFor(() => expect(commands.mcpUninstall).toHaveBeenCalled());
  });

  it('filters the catalog to installed servers', async () => {
    renderRoute();
    await waitFor(() => expect(screen.getByText('Git')).toBeInTheDocument());
    fireEvent.click(installedChip());
    expect(screen.getByText('Git')).toBeInTheDocument();
    expect(screen.queryByText('Filesystem')).not.toBeInTheDocument();
  });
});

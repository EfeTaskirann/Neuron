import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

// Mirror `SwarmRoute.test.tsx`: stub the command surface the route reaches
// (server catalog + install/uninstall mutations), drive the real
// `useServers`/`useMcpInstall`/`useMcpUninstall` hooks through `unwrap`.
// T1-01 render-smoke coverage for the `mcp` tab, which had none before.
vi.mock('../lib/bindings', () => ({
  commands: {
    mcpList: vi.fn(),
    mcpInstall: vi.fn(),
    mcpUninstall: vi.fn(),
  },
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

const FEATURED_SERVER: Server = {
  id: 'srv-fs',
  name: 'Filesystem',
  by: 'Anthropic',
  desc: 'Local file access',
  installs: 12_000,
  rating: 4.8,
  featured: true,
  installed: false,
};

const PLAIN_SERVER: Server = {
  id: 'srv-git',
  name: 'Git',
  by: 'community',
  desc: 'Repo operations',
  installs: 3_400,
  rating: 4.5,
  featured: false,
  installed: true,
};

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.mcpList).mockResolvedValue({
    status: 'ok',
    data: [FEATURED_SERVER, PLAIN_SERVER],
  });
  vi.mocked(commands.mcpInstall).mockResolvedValue({ status: 'ok', data: PLAIN_SERVER });
  vi.mocked(commands.mcpUninstall).mockResolvedValue({ status: 'ok', data: PLAIN_SERVER });
});

describe('MCPRoute', () => {
  it('shows the loading state before the servers query resolves', () => {
    renderRoute();
    expect(screen.getByText(/loading servers/i)).toBeInTheDocument();
  });

  it('renders featured + full catalog with the server count in the search box', async () => {
    renderRoute();
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/search 2 servers/i)).toBeInTheDocument(),
    );
    // A featured server appears twice: once in the Featured strip and once
    // in the "All servers" list; a non-featured one appears only in the list.
    expect(screen.getAllByText('Filesystem').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('Git')).toBeInTheDocument();
    expect(screen.getByText('Featured')).toBeInTheDocument();
    expect(screen.getByText('All servers')).toBeInTheDocument();
  });

  it('renders the empty catalog without throwing', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.mcpList).mockResolvedValue({ status: 'ok', data: [] });
    renderRoute();
    await waitFor(() =>
      expect(screen.getByPlaceholderText(/search 0 servers/i)).toBeInTheDocument(),
    );
  });
});

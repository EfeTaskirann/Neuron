import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClientProvider, QueryClient } from '@tanstack/react-query';
import { App } from './App';

// Mock the bindings layer so tests don't try to reach Tauri's
// `__TAURI_INVOKE`. Each command we exercise gets a per-test
// override via `vi.mocked(commands.X).mockResolvedValueOnce(...)`.
vi.mock('./lib/bindings', () => ({
  commands: {
    meGet: vi.fn(),
  },
}));

// Each test gets its own QueryClient so cache state doesn't leak
// across cases. The provider mirrors `main.tsx` production wiring.
function renderApp(): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <App />
    </QueryClientProvider>,
  );
}

// `useMe` happy-path response — the Sidebar reads `user.initials`,
// `user.name`, `workspace.name`, `workspace.count`.
const ME_OK = {
  status: 'ok' as const,
  data: {
    user: { initials: 'EF', name: 'Efe' },
    workspace: { name: 'Personal', count: 3 },
  },
};

beforeEach(async () => {
  const { commands } = await import('./lib/bindings');
  vi.mocked(commands.meGet).mockResolvedValue(ME_OK);
});

describe('App shell', () => {
  it('renders the sidebar with all 6 nav items', () => {
    renderApp();
    const nav = screen.getByRole('navigation');
    expect(nav).toBeInTheDocument();
    for (const label of ['Workflow', 'Terminal', 'Agents', 'Runs', 'MCP', 'Settings']) {
      expect(screen.getByText(label)).toBeInTheDocument();
    }
  });

  it('clicking a nav item swaps the active route stub', () => {
    renderApp();
    // Default route is canvas (Workflow); click Agents and confirm
    // the stub copy updates to that route.
    fireEvent.click(screen.getByText('Agents'));
    const stub = screen.getByTestId('route-stub-agents');
    expect(stub).toHaveTextContent(/agents.*coming soon/i);
  });

  it('renders user and workspace from useMe()', async () => {
    renderApp();
    // The hook resolves async; wait for the user name to land.
    await waitFor(() => expect(screen.getByText('Efe')).toBeInTheDocument());
    // "Personal" appears in the breadcrumb and the workspace name —
    // assert both occurrences are present rather than picking one.
    expect(screen.getAllByText('Personal').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('3 workflows')).toBeInTheDocument();
    // Initials show twice (workspace badge + footer avatar).
    expect(screen.getAllByText('EF').length).toBeGreaterThanOrEqual(1);
  });
});

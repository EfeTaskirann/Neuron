import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

vi.mock('../lib/bindings', () => ({
  commands: {
    swarmAgentsListStatus: vi.fn(),
  },
}));

// Stub the Tauri event bridge so AgentPane mounts without errors.
vi.mock('@tauri-apps/api/event', () => ({
  listen: () => Promise.resolve(() => {}),
}));

import { SwarmAgentGrid } from './SwarmAgentGrid';
import type { AgentStatusRow } from '../lib/bindings';

function renderGrid(): void {
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={qc}>{children}</QueryClientProvider>;
  }
  render(<SwarmAgentGrid workspaceId="default" />, { wrapper: Wrapper });
}

const ALL_NINE_NOT_SPAWNED: AgentStatusRow[] = [
  'orchestrator',
  'coordinator',
  'scout',
  'planner',
  'backend-builder',
  'frontend-builder',
  'backend-reviewer',
  'frontend-reviewer',
  'integration-tester',
].map((id) => ({
  workspaceId: 'default',
  agentId: id,
  status: 'not_spawned' as const,
  turnsTaken: 0,
  lastActivityMs: null,
}));

beforeEach(async () => {
  const { commands } = await import('../lib/bindings');
  vi.mocked(commands.swarmAgentsListStatus).mockResolvedValue({
    status: 'ok',
    data: ALL_NINE_NOT_SPAWNED,
  });
});

describe('SwarmAgentGrid', () => {
  it('renders all 9 panes by display name', async () => {
    renderGrid();
    // Wait for the status query to resolve so the panes have data.
    await waitFor(() => {
      expect(screen.getByText('Orchestrator')).toBeInTheDocument();
    });
    for (const name of [
      'Orchestrator',
      'Coordinator',
      'Scout',
      'Planner',
      'Backend Builder',
      'Frontend Builder',
      'Backend Reviewer',
      'Frontend Reviewer',
      'Tester',
    ]) {
      expect(screen.getByText(name)).toBeInTheDocument();
    }
  });

  it('queries swarmAgentsListStatus with the workspace id', async () => {
    const { commands } = await import('../lib/bindings');
    renderGrid();
    await waitFor(() =>
      expect(commands.swarmAgentsListStatus).toHaveBeenCalledWith(
        'default',
      ),
    );
  });

  it('renders panes in the documented slot order (alphabetical render is incorrect)', async () => {
    renderGrid();
    await waitFor(() => {
      expect(screen.getByText('Orchestrator')).toBeInTheDocument();
    });
    // Slot order top-down, left-to-right per the WP — the FIRST
    // rendered pane name is Orchestrator (slot 0), the LAST is
    // Tester (slot 8). Pane render order matches DOM order.
    const panes = document.querySelectorAll('.agent-pane');
    expect(panes.length).toBe(9);
    expect(panes[0]?.querySelector('.agent-pane-name')?.textContent).toBe(
      'Orchestrator',
    );
    expect(panes[8]?.querySelector('.agent-pane-name')?.textContent).toBe(
      'Tester',
    );
  });

  it('falls back to not_spawned for agents not in the status response', async () => {
    const { commands } = await import('../lib/bindings');
    // Override the default mock — return only Scout, no other slots.
    vi.mocked(commands.swarmAgentsListStatus).mockResolvedValueOnce({
      status: 'ok',
      data: [
        {
          workspaceId: 'default',
          agentId: 'scout',
          status: 'running',
          turnsTaken: 1,
          lastActivityMs: Date.now(),
        },
      ],
    });
    renderGrid();
    await waitFor(() => {
      expect(screen.getByText('Scout')).toBeInTheDocument();
    });
    // The other 8 slots still render — they just have null status
    // (panes default to NotSpawned-equivalent rendering).
    const panes = document.querySelectorAll('.agent-pane');
    expect(panes.length).toBe(9);
  });
});

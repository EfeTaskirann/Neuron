import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, act, waitFor } from '@testing-library/react';
import type { AgentStatusRow, SwarmAgentEvent } from '../lib/bindings';

// Mock the per-agent event channel so the pane sees a controlled
// event stream. Same handler-map pattern as useAgentEvents.test.tsx.
type Handler = (event: { payload: SwarmAgentEvent }) => void;
const handlers = new Map<string, Handler>();
const unlistenSpies = new Map<string, () => void>();

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: Handler) => {
    handlers.set(channel, handler);
    const unlisten = vi.fn(() => {
      handlers.delete(channel);
    });
    unlistenSpies.set(channel, unlisten);
    return unlisten;
  }),
}));

import { AgentPane } from './AgentPane';

beforeEach(() => {
  handlers.clear();
  unlistenSpies.clear();
});

function fire(channel: string, payload: SwarmAgentEvent): void {
  const h = handlers.get(channel);
  if (h) h({ payload });
}

const NOT_SPAWNED: AgentStatusRow = {
  workspaceId: 'default',
  agentId: 'scout',
  status: 'not_spawned',
  turnsTaken: 0,
  lastActivityMs: null,
};

const RUNNING: AgentStatusRow = {
  workspaceId: 'default',
  agentId: 'scout',
  status: 'running',
  turnsTaken: 1,
  lastActivityMs: Date.now(),
};

describe('AgentPane', () => {
  it('renders the persona name + status pill from props', () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={NOT_SPAWNED}
      />,
    );
    expect(screen.getByText('Scout')).toBeInTheDocument();
    // not_spawned label is "idle" per the AgentStatusPill mapping.
    const pills = screen.getAllByText('idle');
    expect(pills.length).toBeGreaterThan(0);
  });

  it('shows the not-spawned empty hint when no events have fired', () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={NOT_SPAWNED}
      />,
    );
    expect(
      screen.getByText(/idle.*not yet spawned/i),
    ).toBeInTheDocument();
  });

  it('renders an assistant_text bubble when the channel emits one', async () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={RUNNING}
      />,
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      fire('swarm:agent:default:scout:event', {
        kind: 'assistant_text',
        delta: 'merhaba',
      } as SwarmAgentEvent);
    });
    expect(screen.getByText('merhaba')).toBeInTheDocument();
  });

  it('renders a tool_use bubble with name + input summary', async () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={RUNNING}
      />,
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      fire('swarm:agent:default:scout:event', {
        kind: 'tool_use',
        name: 'Read',
        input_summary: 'path: app/src/lib/foo.ts',
      } as SwarmAgentEvent);
    });
    expect(screen.getByText('Read')).toBeInTheDocument();
    expect(
      screen.getByText('path: app/src/lib/foo.ts'),
    ).toBeInTheDocument();
  });

  it('renders the result bubble with cost + turn count', async () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={RUNNING}
      />,
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      fire('swarm:agent:default:scout:event', {
        kind: 'result',
        assistant_text: 'final answer',
        total_cost_usd: 0.0123,
        turn_count: 3,
      } as SwarmAgentEvent);
    });
    expect(screen.getByText('final answer')).toBeInTheDocument();
    // Result row carries "3t · $0.0123" meta. Cost text also lives
    // in the footer mirror, so getAllByText has 2 hits — both fine.
    expect(screen.getByText(/3t/)).toBeInTheDocument();
    const costMatches = screen.getAllByText(/0\.0123/);
    expect(costMatches.length).toBeGreaterThanOrEqual(1);
  });

  it('renders a crashed bubble surfacing the error', async () => {
    const crashed: AgentStatusRow = {
      ...NOT_SPAWNED,
      status: 'crashed',
    };
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={crashed}
      />,
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      fire('swarm:agent:default:scout:event', {
        kind: 'crashed',
        error: 'subprocess died',
      } as SwarmAgentEvent);
    });
    expect(screen.getByText('subprocess died')).toBeInTheDocument();
  });

  it('does not show the turns counter when turnsTaken is 0', () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={NOT_SPAWNED}
      />,
    );
    expect(screen.queryByText('0t')).not.toBeInTheDocument();
  });

  it('shows the turns counter when turnsTaken > 0', () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={{ ...RUNNING, turnsTaken: 5 }}
      />,
    );
    expect(screen.getByText('5t')).toBeInTheDocument();
  });

  it('shows last cost from the most recent result event', async () => {
    render(
      <AgentPane
        workspaceId="default"
        agentId="scout"
        displayName="Scout"
        status={RUNNING}
      />,
    );
    await waitFor(() =>
      expect(handlers.has('swarm:agent:default:scout:event')).toBe(true),
    );
    act(() => {
      // Fire two results — the second should be the displayed cost.
      fire('swarm:agent:default:scout:event', {
        kind: 'result',
        assistant_text: 'r1',
        total_cost_usd: 0.001,
        turn_count: 1,
      } as SwarmAgentEvent);
      fire('swarm:agent:default:scout:event', {
        kind: 'result',
        assistant_text: 'r2',
        total_cost_usd: 0.0042,
        turn_count: 2,
      } as SwarmAgentEvent);
    });
    // Footer cost reflects the latest result.
    const costs = screen.getAllByText(/0\.0042/);
    expect(costs.length).toBeGreaterThan(0);
  });
});

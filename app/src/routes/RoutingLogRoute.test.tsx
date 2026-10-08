import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';

// T1-01 render-smoke for the `routing-log` tab, which had none. The
// route reads from the `useRoutingEvents` hook (a Tauri event listener,
// not a query), so we mock the hook to drive a deterministic event set
// and keep ALL_OUTCOMES / RouteOutcome real so the chip row renders the
// true outcome set.
vi.mock('../hooks/useRoutingEvents', async (importOriginal) => {
  const actual =
    await importOriginal<typeof import('../hooks/useRoutingEvents')>();
  return { ...actual, useRoutingEvents: vi.fn() };
});

import { RoutingLogRoute } from './RoutingLogRoute';
import { useRoutingEvents, type RouteEvent } from '../hooks/useRoutingEvents';

const OK_EVENT: RouteEvent = {
  id: 1,
  source: 'planner',
  target: 'backend-builder',
  body: 'build the API',
  outcome: 'ok',
  ts: 1_700_000_000_000,
};

const DENIED_EVENT: RouteEvent = {
  id: 2,
  source: 'scout',
  target: 'reviewer',
  body: 'denied edge body',
  outcome: 'denied',
  ts: 1_700_000_000_500,
};

function mockEvents(events: RouteEvent[]): { clear: ReturnType<typeof vi.fn> } {
  const clear = vi.fn();
  vi.mocked(useRoutingEvents).mockReturnValue({ events, clear });
  return { clear };
}

beforeEach(() => {
  vi.mocked(useRoutingEvents).mockReset();
});

describe('RoutingLogRoute', () => {
  it('shows the empty-state hint when there are no events', () => {
    mockEvents([]);
    render(<RoutingLogRoute />);
    expect(screen.getByText(/no routing events yet/i)).toBeInTheDocument();
  });

  it('renders one row per event with source → target and body', () => {
    mockEvents([OK_EVENT, DENIED_EVENT]);
    render(<RoutingLogRoute />);
    expect(screen.getByText('@planner')).toBeInTheDocument();
    expect(screen.getByText('@backend-builder')).toBeInTheDocument();
    expect(screen.getByText('build the API')).toBeInTheDocument();
    expect(screen.getByText('denied edge body')).toBeInTheDocument();
  });

  it('filters out an outcome when its chip is toggled off', () => {
    mockEvents([OK_EVENT, DENIED_EVENT]);
    render(<RoutingLogRoute />);
    // The `ok` chip is labelled "routed"; toggling it off drops the ok row.
    fireEvent.click(screen.getByRole('button', { name: 'routed' }));
    expect(screen.queryByText('build the API')).not.toBeInTheDocument();
    expect(screen.getByText('denied edge body')).toBeInTheDocument();
  });

  it('renders bridge notices / rejections / reminders as their own outcomes', () => {
    // Previously these arrived as unknown outcomes and were coerced to
    // "no target" / "malformed", hiding what the bridge actually did.
    mockEvents([
      { id: 3, source: 'neuron', target: 'scout', body: '[Neuron] not delivered', outcome: 'notice', ts: 1 },
      { id: 4, source: 'coordinator', target: 'planner', body: 'plan-07', outcome: 'rejected', ts: 2 },
      { id: 5, source: 'neuron', target: 'frontend-reviewer', body: '[Neuron] hatırlatma', outcome: 'reminder', ts: 3 },
    ]);
    const { container } = render(<RoutingLogRoute />);
    expect(container.querySelector('.routing-log-tag-notice')).toHaveTextContent('notice');
    expect(container.querySelector('.routing-log-tag-rejected')).toHaveTextContent('rejected');
    expect(container.querySelector('.routing-log-tag-reminder')).toHaveTextContent('reminder');
    // Each has its own filter chip.
    fireEvent.click(screen.getByRole('button', { name: 'notice' }));
    expect(screen.queryByText('[Neuron] not delivered')).not.toBeInTheDocument();
    expect(screen.getByText('plan-07')).toBeInTheDocument();
  });

  it('calls clear when the Clear button is pressed with events present', () => {
    const { clear } = mockEvents([OK_EVENT]);
    render(<RoutingLogRoute />);
    fireEvent.click(screen.getByRole('button', { name: /^clear$/i }));
    expect(clear).toHaveBeenCalledTimes(1);
  });
});

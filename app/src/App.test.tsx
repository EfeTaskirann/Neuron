import { describe, expect, it } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { QueryClientProvider, QueryClient } from '@tanstack/react-query';
import { App } from './App';

// Each test gets its own QueryClient so cache state doesn't leak
// across cases. The provider is required because phase-A `App`
// renders inside a QueryClientProvider in production via main.tsx;
// even though no hooks are mounted in phase A, sub-trees added in
// phase B will rely on the provider being present.
function renderApp(): void {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={qc}>
      <App />
    </QueryClientProvider>,
  );
}

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
});

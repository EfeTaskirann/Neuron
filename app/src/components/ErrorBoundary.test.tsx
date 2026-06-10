import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, screen } from '@testing-library/react';
import { ErrorBoundary } from './ErrorBoundary';

// T1-02 — route-level ErrorBoundary smoke. Each of the 9 routes is
// wrapped in <ErrorBoundary fallbackTitle="…"> (App.tsx::RouteHost),
// so a render-time throw (typically a TanStack Query error surfaced via
// throwOnError) must degrade to a recoverable card rather than crashing
// the shell. This proves that contract once, generically.

// A child that throws on render so the boundary catches it.
function Boom(): JSX.Element {
  throw new Error('kaboom from child');
}

describe('ErrorBoundary', () => {
  // React + the boundary both log the caught error; silence the noise
  // so the suite output stays clean — we assert on the rendered
  // fallback, not the console.
  let errorSpy: ReturnType<typeof vi.spyOn>;
  beforeEach(() => {
    errorSpy = vi.spyOn(console, 'error').mockImplementation(() => {});
  });
  afterEach(() => {
    errorSpy.mockRestore();
  });

  it('renders its children when nothing throws', () => {
    render(
      <ErrorBoundary fallbackTitle="Couldn't load thing">
        <div>healthy child</div>
      </ErrorBoundary>,
    );
    expect(screen.getByText('healthy child')).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('catches a render error and shows the recoverable fallback', () => {
    render(
      <ErrorBoundary fallbackTitle="Couldn't load agents">
        <Boom />
      </ErrorBoundary>,
    );
    // The alert role + route-specific title + thrown message all surface.
    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(screen.getByText("Couldn't load agents")).toBeInTheDocument();
    expect(screen.getByText('kaboom from child')).toBeInTheDocument();
    // The retry affordance is present so the user can recover.
    expect(screen.getByRole('button', { name: /retry/i })).toBeInTheDocument();
  });

  it('falls back to the default title when none is provided', () => {
    render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(screen.getByText('Something went wrong')).toBeInTheDocument();
  });
});

// Route-level error boundary for the data-driven UI. Catches render
// errors thrown by hooks (typically a TanStack Query error surfaced
// via `throwOnError`) and renders a recoverable panel. Retry resets
// the React tree by remounting children under a new `key` so any
// stale component state is gone.
import { Component, type ErrorInfo, type ReactNode } from 'react';
import { queryClient } from '../lib/queryClient';

interface ErrorBoundaryProps {
  children: ReactNode;
  fallbackTitle?: string;
}

interface ErrorBoundaryState {
  error: Error | null;
  resetKey: number;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null, resetKey: 0 };

  static getDerivedStateFromError(error: Error): Partial<ErrorBoundaryState> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // Surfacing into the console is enough for Week 2 — Week 3
    // wires this into the centralised tracing pipeline.
    console.error('[ErrorBoundary]', error, info.componentStack);
  }

  private handleRetry = (): void => {
    queryClient.resetQueries();
    this.setState((prev) => ({ error: null, resetKey: prev.resetKey + 1 }));
  };

  render(): ReactNode {
    const { error, resetKey } = this.state;
    const { children, fallbackTitle = 'Something went wrong' } = this.props;
    if (error) {
      return (
        <div className="error-boundary" role="alert">
          <div className="error-boundary-card">
            <h2>{fallbackTitle}</h2>
            <p className="error-boundary-message">{error.message || String(error)}</p>
            <button type="button" className="btn-primary" onClick={this.handleRetry}>
              Retry
            </button>
          </div>
        </div>
      );
    }
    return <div key={resetKey}>{children}</div>;
  }
}

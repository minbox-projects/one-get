import { Component } from "react";
import type { ReactNode } from "react";

interface ErrorBoundaryProps {
  children?: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
}

export class ErrorBoundary extends Component<
  ErrorBoundaryProps,
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  render() {
    if (this.state.hasError) {
      return (
        <main className="app-shell">
          <div className="error-boundary" role="alert">
            <h1>Something went wrong.</h1>
            <p>Reload OneGet and try again.</p>
          </div>
        </main>
      );
    }

    return this.props.children;
  }
}

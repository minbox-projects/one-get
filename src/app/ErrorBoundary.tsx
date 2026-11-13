import { Component } from "react";
import type { ReactNode } from "react";

import { resolveLanguage, translate } from "../i18n";

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
      const language = resolveLanguage("system");
      return (
        <main className="app-shell">
          <div className="error-boundary" role="alert">
            <h1>{translate(language, "error.somethingWentWrong")}</h1>
            <p>{translate(language, "error.reload")}</p>
          </div>
        </main>
      );
    }

    return this.props.children;
  }
}

import React from "react";

interface ErrorBoundaryState {
  hasError: boolean;
  message: string;
}

export class ErrorBoundary extends React.Component<
  { children: React.ReactNode },
  ErrorBoundaryState
> {
  state: ErrorBoundaryState = {
    hasError: false,
    message: "",
  };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return {
      hasError: true,
      message: error.message || "Unknown error",
    };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo): void {
    // Keep diagnostics in console for debugging.
    console.error("ClawDesk UI crashed", error, info.componentStack);
  }

  render(): React.ReactNode {
    if (!this.state.hasError) {
      return this.props.children;
    }
    return (
      <div className="fatal-screen">
        <div className="section-card fatal-card">
          <h2>ClawDesk hit a UI error.</h2>
          <p>Reload to recover the session.</p>
          <p className="fatal-detail">{this.state.message}</p>
          <button
            className="btn primary"
            onClick={() => window.location.reload()}
          >
            Reload App
          </button>
        </div>
      </div>
    );
  }
}

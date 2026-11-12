import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import App from "./App";
import { ErrorBoundary } from "./app/ErrorBoundary";

function ThrowingChild(): never {
  throw new Error("diagnostics render failure");
}

describe("App shell", () => {
  it("renders the OneGet shell heading and diagnostics region", () => {
    render(<App />);

    expect(
      screen.getByRole("heading", { level: 1, name: "OneGet" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: /diagnostics/i }),
    ).toBeInTheDocument();
  });

  it("shows a safe message when a child throws instead of crashing", () => {
    render(
      <ErrorBoundary>
        <ThrowingChild />
      </ErrorBoundary>,
    );

    expect(
      screen.getByRole("alert"),
    ).toHaveTextContent(/something went wrong/i);
    expect(screen.queryByText("diagnostics render failure")).not.toBeInTheDocument();
  });
});

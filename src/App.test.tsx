import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App from "./App";
import { ErrorBoundary } from "./app/ErrorBoundary";
import { dictionaries } from "./i18n";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("./lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

const en = dictionaries["en-US"];

function ThrowingChild(): never {
  throw new Error("diagnostics render failure");
}

beforeEach(() => {
  invokeCommand.mockReset();
  invokeCommand.mockResolvedValue({
    ok: true,
    data: {
      schemaVersion: 2,
      language: "en-US",
      theme: "system",
      closeBehavior: "tray",
      defaultDirectory: null,
      defaultConnections: 8,
      maxConcurrentTasks: 3,
      globalRateLimit: null,
      logLevel: "info",
    },
  });
});

describe("App shell", () => {
  it("renders the task-first shell", async () => {
    render(<App />);

    expect(
      screen.getByRole("heading", { level: 1, name: en["app.title"] }),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("navigation", { name: en["nav.label"] }),
    ).toBeInTheDocument();
    expect(await screen.findByText(en["list.emptyTitle"])).toBeInTheDocument();
  });

  it("shows a safe message when a child throws instead of crashing", () => {
    render(
      <ErrorBoundary>
        <ThrowingChild />
      </ErrorBoundary>,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(/something went wrong/i);
    expect(screen.queryByText("diagnostics render failure")).not.toBeInTheDocument();
  });
});

import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { DiagnosticsPanel } from "./DiagnosticsPanel";
import { err, ok, runtimeUnavailable } from "../../test/mocks/tauri";

const { invokeCommand, isTauriRuntime } = vi.hoisted(() => ({
  invokeCommand: vi.fn(),
  isTauriRuntime: vi.fn(() => false),
}));

vi.mock("../../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime,
}));

const appInfo = {
  name: "OneGet",
  version: "0.1.0",
  logPath: "/tmp/oneget.log",
};

const settings = {
  schemaVersion: 1,
  theme: "system" as const,
  logLevel: "info" as const,
};

function commandResult(command: string, args?: Record<string, unknown>) {
  switch (command) {
    case "app_info":
      return ok(appInfo);
    case "settings_get":
      return ok(settings);
    case "settings_save":
      return ok(args?.settings ?? settings);
    case "deep_link_last":
      return ok({ accepted: false });
    case "diagnostics_autostart_get":
      return ok({ enabled: false });
    case "diagnostics_autostart_set":
      return ok({ enabled: Boolean(args?.enabled) });
    case "diagnostics_notification_status":
      return ok({ permission: "granted" });
    case "diagnostics_send_notification":
      return ok({ sent: true });
    case "diagnostics_secret_roundtrip":
      return ok({ success: true });
    case "app_graceful_quit":
      return ok({ quitting: true });
    default:
      return err("unexpected_command", `Unhandled command: ${command}`);
  }
}

describe("DiagnosticsPanel", () => {
  beforeEach(() => {
    invokeCommand.mockReset();
    isTauriRuntime.mockReset();
    isTauriRuntime.mockReturnValue(false);
    isTauriRuntime.mockReturnValue(true);
    invokeCommand.mockImplementation(async (command: string, args?: Record<string, unknown>) =>
      commandResult(command, args),
    );
  });

  it("loads and displays app information", async () => {
    render(<DiagnosticsPanel />);

    expect(await screen.findByText("OneGet")).toBeInTheDocument();
    expect(screen.getByText("0.1.0")).toBeInTheDocument();
    expect(screen.getByText("/tmp/oneget.log")).toBeInTheDocument();
  });

  it("shows a runtime-unavailable banner when commands report runtime_unavailable", async () => {
    invokeCommand.mockResolvedValue(runtimeUnavailable());

    render(<DiagnosticsPanel />);

    expect(await screen.findByText(/runtime unavailable/i)).toBeInTheDocument();
  });

  it("toggles autostart with the new boolean and reflects returned state", async () => {
    const user = userEvent.setup();
    let enabled = false;
    invokeCommand.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "diagnostics_autostart_get") {
        return ok({ enabled });
      }
      if (command === "diagnostics_autostart_set") {
        enabled = Boolean(args?.enabled);
        return ok({ enabled });
      }
      return commandResult(command, args);
    });

    render(<DiagnosticsPanel />);

    const enableButton = await screen.findByRole("button", { name: "Enable autostart" });
    await user.click(enableButton);

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("diagnostics_autostart_set", { enabled: true });
    });
    expect(
      await screen.findByRole("button", { name: "Disable autostart" }),
    ).toBeInTheDocument();
  });

  it("shows typed notification denial code and message", async () => {
    const user = userEvent.setup();
    invokeCommand.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "diagnostics_send_notification") {
        return err("notification_denied", "Notification permission was denied.");
      }
      return commandResult(command, args);
    });

    render(<DiagnosticsPanel />);

    await user.click(await screen.findByRole("button", { name: "Send test notification" }));

    expect(await screen.findByText(/notification_denied/i)).toBeInTheDocument();
    expect(screen.getByText(/Notification permission was denied\./i)).toBeInTheDocument();
  });

  it("reports secret round-trip success or failure without rendering the secret value", async () => {
    const user = userEvent.setup();
    let capturedSecret: unknown;
    invokeCommand.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "diagnostics_secret_roundtrip") {
        capturedSecret = args?.secret;
        return ok({ success: true });
      }
      return commandResult(command, args);
    });

    const { container } = render(<DiagnosticsPanel />);

    await user.click(await screen.findByRole("button", { name: "Run secret round-trip" }));

    expect(await screen.findByText(/success/i)).toBeInTheDocument();
    expect(typeof capturedSecret).toBe("string");
    expect(capturedSecret).not.toBe("");
    expect(container.textContent).not.toContain(capturedSecret as string);
  });

  it("persists theme and log level changes via settings_save", async () => {
    const user = userEvent.setup();

    render(<DiagnosticsPanel />);

    const themeControl = await screen.findByLabelText("Theme");
    await user.selectOptions(themeControl, "dark");

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith(
        "settings_save",
        expect.objectContaining({
          settings: expect.objectContaining({ theme: "dark" }),
        }),
      );
    });

    const logLevelControl = screen.getByLabelText("Log level");
    await user.selectOptions(logLevelControl, "debug");

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith(
        "settings_save",
        expect.objectContaining({
          settings: expect.objectContaining({ logLevel: "debug" }),
        }),
      );
    });
  });

  it("calls app_graceful_quit without letting raw errors escape", async () => {
    const user = userEvent.setup();
    invokeCommand.mockImplementation(async (command: string, args?: Record<string, unknown>) => {
      if (command === "app_graceful_quit") {
        return err("quit_failed", "Unable to quit.");
      }
      return commandResult(command, args);
    });

    render(<DiagnosticsPanel />);

    await user.click(await screen.findByRole("button", { name: "Quit OneGet" }));

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("app_graceful_quit");
    });
    expect(await screen.findByText(/quit_failed/i)).toBeInTheDocument();
    expect(screen.getByText(/Unable to quit\./i)).toBeInTheDocument();
  });
});

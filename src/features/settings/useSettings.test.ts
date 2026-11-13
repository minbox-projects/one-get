import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppError, Settings } from "../../types/app";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

import { useSettings } from "./useSettings";

const status = { readOnly: false, recovered: false };

function settings(overrides: Partial<Settings> = {}): Settings {
  return {
    schemaVersion: 2,
    language: "en-US",
    theme: "system",
    closeBehavior: "tray",
    defaultDirectory: null,
    defaultConnections: 8,
    maxConcurrentTasks: 3,
    globalRateLimit: null,
    logLevel: "info",
    ...overrides,
  };
}

function commands(): string[] {
  return invokeCommand.mock.calls.map(([command]) => command as string);
}

function loadSettings(
  value: Settings,
  nextStatus: typeof status = status,
): void {
  invokeCommand.mockImplementation(async (command: string) => {
    if (command === "settings_get") {
      return { ok: true as const, data: value };
    }
    if (command === "settings_status") {
      return { ok: true as const, data: nextStatus };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

describe("useSettings", () => {
  beforeEach(() => {
    invokeCommand.mockReset();
  });

  it("loads settings and status through their typed commands", async () => {
    const value = settings({ theme: "dark", customField: "kept" });
    loadSettings(value);

    const { result } = renderHook(() => useSettings());

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.settings).toEqual(value);
    expect(result.current.status).toEqual(status);
    expect(result.current.error).toBeNull();
    expect(commands()).toEqual(
      expect.arrayContaining(["settings_get", "settings_status"]),
    );
  });

  it("saves schema-2 settings and returns success", async () => {
    const initial = settings();
    const saved = settings({ theme: "dark", defaultConnections: 16 });
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: initial };
      }
      if (command === "settings_status") {
        return { ok: true as const, data: status };
      }
      if (command === "settings_save") {
        return { ok: true as const, data: saved };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    const { result } = renderHook(() => useSettings());
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    let outcome = false;
    await act(async () => {
      outcome = await result.current.save(saved);
    });

    expect(outcome).toBe(true);
    expect(invokeCommand).toHaveBeenCalledWith("settings_save", {
      settings: saved,
    });
    expect(result.current.settings).toEqual(saved);
    expect(result.current.saving).toBe(false);
  });

  it("surfaces a typed save error without replacing the loaded settings", async () => {
    const value = settings({ theme: "light" });
    const failure: AppError = {
      code: "settings_write_failed",
      message: "Settings could not be saved.",
    };
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: value };
      }
      if (command === "settings_status") {
        return { ok: true as const, data: status };
      }
      if (command === "settings_save") {
        return { ok: false as const, error: failure };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    const { result } = renderHook(() => useSettings());
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });

    let outcome = true;
    await act(async () => {
      outcome = await result.current.save(settings({ theme: "dark" }));
    });

    expect(outcome).toBe(false);
    expect(result.current.error).toEqual(failure);
    expect(result.current.settings).toEqual(value);
  });

  it("reloads settings and status while exposing read-only and recovered state", async () => {
    const first = settings({ theme: "system" });
    const second = settings({ theme: "light", defaultDirectory: "/tmp/out" });
    let loadCount = 0;
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        loadCount += 1;
        return {
          ok: true as const,
          data: loadCount === 1 ? first : second,
        };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: true, recovered: true },
        };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    const { result } = renderHook(() => useSettings());
    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.settings).toEqual(first);
    expect(result.current.status).toEqual({ readOnly: true, recovered: true });

    await act(async () => {
      await result.current.reload();
    });

    expect(result.current.settings).toEqual(second);
    expect(
      commands().filter((command) => command === "settings_get"),
    ).toHaveLength(2);
    expect(
      commands().filter((command) => command === "settings_status"),
    ).toHaveLength(2);
  });

  it("reports a typed load error and leaves settings empty", async () => {
    const failure: AppError = {
      code: "settings_read_failed",
      message: "Settings could not be read.",
    };
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: false as const, error: failure };
      }
      if (command === "settings_status") {
        return { ok: true as const, data: status };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    const { result } = renderHook(() => useSettings());

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.settings).toBeNull();
    expect(result.current.error).toEqual(failure);
  });
});

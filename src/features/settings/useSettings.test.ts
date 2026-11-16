import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { AppError, Settings } from "../../types/app";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

import { useSettings } from "./useSettings";

const status = {
  readOnly: false,
  recovered: false,
  proxyUnavailableReason: null as string | null,
};

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
    proxyMode: "direct",
    proxyType: "http",
    proxyHost: null,
    proxyPort: null,
    defaultCookiesFile: null,
    userAgent: "OneGet-Test/1.0",
    connectTimeoutSecs: 15,
    readTimeoutSecs: 30,
    retryAttempts: 5,
    hfConnectionCap: 8,
    alwaysAskQuality: true,
    videoQualityPreference: "mp4Compatible",
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

  it("loads the system-proxy unavailable reason reported by settings_status", async () => {
    const value = settings();
    const withReason = {
      readOnly: false,
      recovered: false,
      proxyUnavailableReason: "SYSTEM_PROXY_PAC_ONLY",
    };
    loadSettings(value, withReason);

    const { result } = renderHook(() => useSettings());

    await waitFor(() => {
      expect(result.current.loading).toBe(false);
    });
    expect(result.current.status).toEqual(withReason);
    expect(result.current.settings).toEqual(value);
    expect(result.current.error).toBeNull();
  });

  it("keeps read-only and recovered state when status fails without crashing", async () => {
    const failure: AppError = {
      code: "settings_status_failed",
      message: "Settings status could not be read.",
    };
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: settings() };
      }
      if (command === "settings_status") {
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
    expect(result.current.settings).not.toBeNull();
    expect(result.current.status).toBeNull();
  });

  it("returns the backend document from save and keeps live language and theme unchanged", async () => {
    const initial = settings({ theme: "system", language: "en-US" });
    const backendDocument = settings({
      theme: "dark",
      language: "zh-CN",
      proxyMode: "manual",
      proxyType: "socks5",
      proxyHost: "proxy.example.com",
      proxyPort: 1080,
      defaultCookiesFile: "/tmp/oneget-cookies.txt",
      userAgent: "OneGet-Test/9.9",
      connectTimeoutSecs: 30,
      readTimeoutSecs: 120,
      retryAttempts: 7,
      hfConnectionCap: 16,
      alwaysAskQuality: false,
      videoQualityPreference: "highest",
      customField: "kept",
    });
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: initial };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: true, recovered: true, proxyUnavailableReason: null },
        };
      }
      if (command === "settings_save") {
        return { ok: true as const, data: backendDocument };
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
      outcome = await result.current.save(
        settings({ theme: "dark", language: "zh-CN" }),
      );
    });

    expect(outcome).toBe(true);
    expect(result.current.settings).toEqual(backendDocument);
    expect(result.current.settings).toMatchObject({
      language: "zh-CN",
      theme: "dark",
      proxyMode: "manual",
      videoQualityPreference: "highest",
      customField: "kept",
    });
    expect(result.current.status).toEqual({
      readOnly: true,
      recovered: true,
      proxyUnavailableReason: null,
    });
  });
});

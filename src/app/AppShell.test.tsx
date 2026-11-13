import { render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../i18n";
import { AppShell } from "./AppShell";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

type SettingsOverrides = Partial<Record<string, unknown>>;

function settings(overrides: SettingsOverrides = {}) {
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

function renderShell() {
  return render(
    <I18nProvider language="en-US">
      <AppShell />
    </I18nProvider>,
  );
}

function mockSettings(overrides: SettingsOverrides = {}) {
  invokeCommand.mockImplementation(async (command: string) => {
    if (command === "settings_get") {
      return { ok: true as const, data: settings(overrides) };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

beforeEach(() => {
  invokeCommand.mockReset();
});

afterEach(() => {
  delete document.documentElement.dataset.theme;
});

describe("AppShell", () => {
  it("renders the task-first sidebar, toolbar, list, details, and status bar", async () => {
    mockSettings();
    renderShell();

    expect(await screen.findByText(en["list.emptyTitle"])).toBeInTheDocument();
    expect(screen.getByText(en["list.emptyBody"])).toBeInTheDocument();

    const nav = screen.getByRole("navigation", { name: en["nav.label"] });
    const navKeys = [
      "nav.all",
      "nav.downloading",
      "nav.completed",
      "nav.failed",
      "nav.video",
      "nav.model",
      "nav.document",
      "nav.program",
      "nav.other",
    ];
    for (const key of navKeys) {
      expect(within(nav).getByText(en[key])).toBeInTheDocument();
    }
    expect(nav).toHaveTextContent("0");

    for (const key of [
      "toolbar.new",
      "toolbar.batch",
      "toolbar.pauseAll",
      "toolbar.resumeAll",
      "toolbar.settings",
    ]) {
      expect(screen.getByRole("button", { name: en[key] })).toBeInTheDocument();
    }
    expect(screen.getByLabelText(en["toolbar.search"])).toBeInTheDocument();

    expect(screen.getByText(en["details.label"])).toBeInTheDocument();
    expect(screen.getByText(en["details.empty"])).toBeInTheDocument();

    expect(screen.getByText(en["status.active"])).toBeInTheDocument();
    expect(screen.getByText(en["status.speed"])).toBeInTheDocument();
    expect(screen.getAllByText(en["common.unknown"]).length).toBeGreaterThan(0);
  });

  it("loads settings once, shows a skeleton, then the empty state", async () => {
    let resolveSettings!: (value: unknown) => void;
    invokeCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveSettings = resolve;
      }),
    );

    renderShell();

    const list = screen.getByRole("region", { name: en["list.label"] });
    expect(list).toHaveAttribute("aria-busy", "true");

    resolveSettings({ ok: true, data: settings() });

    expect(await screen.findByText(en["list.emptyTitle"])).toBeInTheDocument();
    expect(list).not.toHaveAttribute("aria-busy", "true");
    expect(
      invokeCommand.mock.calls.filter(([command]) => command === "settings_get"),
    ).toHaveLength(1);
  });

  it("shows the localized runtime-unavailable fallback when IPC fails", async () => {
    invokeCommand.mockResolvedValue({
      ok: false,
      error: {
        code: "runtime_unavailable",
        message: "Tauri runtime is unavailable in this environment.",
      },
    });

    renderShell();

    expect(await screen.findByText(en["error.runtimeUnavailable"])).toBeInTheDocument();
  });

  it("applies the theme from settings to the document root", async () => {
    mockSettings({ theme: "dark" });
    renderShell();

    await screen.findByText(en["list.emptyTitle"]);

    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("renders Chinese dictionary strings when settings select zh-CN", async () => {
    mockSettings({ language: "zh-CN" });
    renderShell();

    expect(await screen.findByText(zh["list.emptyTitle"])).toBeInTheDocument();
    expect(screen.getByText(zh["nav.all"])).toBeInTheDocument();
    expect(screen.getByText(zh["details.empty"])).toBeInTheDocument();
  });
});

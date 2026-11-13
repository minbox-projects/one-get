import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../i18n";
import type { DownloadSummary } from "../types/downloads";

import { AppShell } from "./AppShell";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

function dictionaryMessage(
  dictionary: Record<string, string>,
  key: string,
): string {
  expect(dictionary[key], `missing dictionary key ${key}`).toBeTruthy();
  return dictionary[key];
}

function settings(overrides: Record<string, unknown> = {}) {
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

function task(overrides: Partial<DownloadSummary> = {}): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/files/report.pdf",
    filename: "report.pdf",
    destination: "/Users/me/Downloads",
    state: "downloading",
    downloadedBytes: 0,
    totalBytes: 100,
    connections: 8,
    createdAtMs: 1,
    updatedAtMs: 1,
    completedAtMs: null,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

function mockIpc(
  downloads: DownloadSummary[],
  overrides: Record<string, unknown> = {},
  language: "en-US" | "zh-CN" = "en-US",
) {
  invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
    if (command === "settings_get") {
      return { ok: true as const, data: settings({ language }) };
    }
    if (command === "settings_status") {
      return { ok: true as const, data: { readOnly: false, recovered: false } };
    }
    if (command === "settings_save") {
      return {
        ok: true as const,
        data: (args as { settings: Record<string, unknown> }).settings,
      };
    }
    if (command === "downloads_list") {
      return { ok: true as const, data: downloads };
    }
    if (command === "downloads_segments") {
      return { ok: true as const, data: [] };
    }
    if (command in overrides) {
      return overrides[command];
    }
    if (command.startsWith("downloads_")) {
      return { ok: true as const, data: undefined };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

function renderShell() {
  return render(
    <I18nProvider language="en-US">
      <AppShell />
    </I18nProvider>,
  );
}

async function selectTask(
  user: ReturnType<typeof userEvent.setup>,
  filename: string,
) {
  await screen.findByText(filename);
  await user.click(screen.getByText(filename));
}

function detailsRegion(language: "en-US" | "zh-CN" = "en-US") {
  return screen.getByRole("region", {
    name: dictionaries[language]["details.label"],
  });
}

beforeEach(() => {
  invokeCommand.mockReset();
});

afterEach(() => {
  delete (navigator as { clipboard?: unknown }).clipboard;
  delete document.documentElement.dataset.theme;
});

describe("AppShell success feedback", () => {
  it("shows a localized copied-URL toast with a localized dismiss action", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    mockIpc([task({ state: "completed", completedAtMs: 2 })]);
    renderShell();

    await selectTask(user, "report.pdf");
    await user.click(
      within(detailsRegion()).getByRole("button", {
        name: en["details.copyUrl"],
      }),
    );

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledTimes(1);
    });
    expect(
      await screen.findByText(dictionaryMessage(en, "details.copied")),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", {
        name: dictionaryMessage(en, "common.dismiss"),
      }),
    ).toBeInTheDocument();
  });

  it("shows a localized success toast after pausing a task", async () => {
    const user = userEvent.setup();
    mockIpc([task({ state: "downloading" })]);
    renderShell();

    await selectTask(user, "report.pdf");
    await user.click(
      within(detailsRegion()).getByRole("button", { name: en["task.pause"] }),
    );

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_pause", {
        id: "task-1",
      });
    });
    expect(
      await screen.findByText(dictionaryMessage(en, "toast.paused")),
    ).toBeInTheDocument();
  });

  it("shows a localized success toast after deleting a record", async () => {
    const user = userEvent.setup();
    mockIpc([task({ state: "completed", completedAtMs: 2 })]);
    renderShell();

    await selectTask(user, "report.pdf");
    await user.click(
      within(detailsRegion()).getByRole("button", {
        name: en["delete.recordConfirm"],
      }),
    );
    const dialog = await screen.findByRole("dialog");
    await user.click(
      within(dialog).getByRole("button", { name: en["delete.recordConfirm"] }),
    );

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_delete_record", {
        id: "task-1",
      });
    });
    expect(
      await screen.findByText(dictionaryMessage(en, "toast.deleted")),
    ).toBeInTheDocument();
  });

  it("shows a localized success toast after saving settings", async () => {
    const user = userEvent.setup();
    mockIpc([]);
    renderShell();
    await screen.findByText(en["list.emptyTitle"]);

    await user.click(
      screen.getByRole("button", { name: en["toolbar.settings"] }),
    );
    const dialog = await screen.findByRole("dialog", {
      name: en["settings.title"],
    });
    await within(dialog).findByLabelText(/language/i);
    await user.click(within(dialog).getByRole("button", { name: /^save/i }));

    await waitFor(() => {
      expect(
        invokeCommand.mock.calls.some(
          ([command]) => command === "settings_save",
        ),
      ).toBe(true);
    });
    expect(
      await screen.findByText(dictionaryMessage(en, "toast.settingsSaved")),
    ).toBeInTheDocument();
  });

  it("uses Chinese success and dismiss text when the interface language is zh-CN", async () => {
    const user = userEvent.setup();
    mockIpc([task({ state: "downloading" })], {}, "zh-CN");
    renderShell();

    await screen.findByRole("button", { name: zh["toolbar.new"] });
    await selectTask(user, "report.pdf");
    await user.click(
      within(detailsRegion("zh-CN")).getByRole("button", {
        name: zh["task.pause"],
      }),
    );

    const paused = dictionaryMessage(zh, "toast.paused");
    expect(paused).not.toBe(dictionaryMessage(en, "toast.paused"));
    expect(await screen.findByText(paused)).toBeInTheDocument();

    const dismiss = dictionaryMessage(zh, "common.dismiss");
    expect(dismiss).not.toBe("Dismiss");
    expect(screen.getByRole("button", { name: dismiss })).toBeInTheDocument();
  });
});

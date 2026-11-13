import { act, render, screen, waitFor, within } from "@testing-library/react";
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

function task(overrides: Partial<DownloadSummary> = {}): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/file.bin",
    filename: "file.bin",
    destination: null,
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

function largeTaskSet(count: number): DownloadSummary[] {
  return Array.from({ length: count }, (_, index) =>
    task({
      id: `task-${index}`,
      url: `https://files.example.test/file-${index}.mp4`,
      filename: `file-${index}.mp4`,
      state: index < 3 ? "downloading" : "completed",
      totalBytes: 1000,
      downloadedBytes: 500,
      createdAtMs: index,
      updatedAtMs: index,
      completedAtMs: index < 3 ? null : index,
    }),
  );
}

function renderShell() {
  return render(
    <I18nProvider language="en-US">
      <AppShell />
    </I18nProvider>,
  );
}

function mockIpc(
  downloads: DownloadSummary[],
  overrides: SettingsOverrides = {},
) {
  invokeCommand.mockImplementation(async (command: string) => {
    if (command === "settings_get") {
      return { ok: true as const, data: settings(overrides) };
    }
    if (command === "downloads_list") {
      return { ok: true as const, data: downloads };
    }
    return {
      ok: false as const,
      error: { code: "unexpected_command", message: command },
    };
  });
}

async function renderWithSettledTimers(downloads: DownloadSummary[]) {
  vi.useFakeTimers();
  mockIpc(downloads);
  const view = renderShell();
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(0);
  });
  return view;
}

beforeEach(() => {
  invokeCommand.mockReset();
});

afterEach(() => {
  vi.useRealTimers();
  delete document.documentElement.dataset.theme;
});

describe("AppShell", () => {
  it("renders the task-first sidebar, toolbar, list, details, and status bar", async () => {
    mockIpc([]);
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

  it("shows a loading skeleton while downloads are pending, then the empty state", async () => {
    let resolveDownloads!: (value: unknown) => void;
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: settings() };
      }
      if (command === "downloads_list") {
        return new Promise((resolve) => {
          resolveDownloads = resolve;
        });
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    renderShell();

    const list = screen.getByRole("region", { name: en["list.label"] });
    expect(list).toHaveAttribute("aria-busy", "true");
    expect(within(list).getByRole("status")).toBeInTheDocument();
    expect(screen.queryAllByTestId("task-row")).toHaveLength(0);
    expect(
      invokeCommand.mock.calls.filter(([command]) => command === "settings_get"),
    ).toHaveLength(1);
    await waitFor(() => {
      expect(resolveDownloads).toBeTypeOf("function");
    });

    resolveDownloads({ ok: true, data: [] });

    expect(await screen.findByText(en["list.emptyTitle"])).toBeInTheDocument();
    expect(list).not.toHaveAttribute("aria-busy", "true");
  });

  it("shows a filtered-empty state instead of the empty state when a search matches nothing", async () => {
    const user = userEvent.setup();
    mockIpc([
      task({ id: "a", filename: "alpha.mp4" }),
      task({ id: "b", filename: "beta.mp4" }),
    ]);
    renderShell();

    await screen.findByText("alpha.mp4");
    await user.type(screen.getByLabelText(en["toolbar.search"]), "zzz-no-match");

    await waitFor(() => {
      expect(screen.queryAllByTestId("task-row")).toHaveLength(0);
    });

    expect(screen.queryByText(en["list.emptyTitle"])).not.toBeInTheDocument();
    const list = screen.getByRole("region", { name: en["list.label"] });
    expect(
      within(list).getByText(
        /no (downloads? )?(match|matching)|nothing (matches|found)|no results/i,
      ),
    ).toBeInTheDocument();
  });

  it("renders 10,000 tasks with bounded rows, default order, counts, and status totals", async () => {
    await renderWithSettledTimers(largeTaskSet(10_000));

    const rows = screen.getAllByTestId("task-row");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(100);
    expect(within(rows[0]).getByText("file-9999.mp4")).toBeInTheDocument();

    const nav = screen.getByRole("navigation", { name: en["nav.label"] });
    expect(
      within(nav).getByRole("button", { name: new RegExp(en["nav.all"]) }),
    ).toHaveTextContent("10,000");
    expect(
      within(nav).getByRole("button", { name: new RegExp(en["nav.video"]) }),
    ).toHaveTextContent("10,000");
    expect(
      within(nav).getByRole("button", {
        name: new RegExp(en["nav.downloading"]),
      }),
    ).toHaveTextContent("3");

    const active = screen.getByText(en["status.active"]).parentElement;
    expect(active).not.toBeNull();
    expect(active).toHaveTextContent("3");

    const speed = screen.getByText(en["status.speed"]).parentElement;
    expect(speed).not.toBeNull();
    expect(speed).toHaveTextContent(en["common.unknown"]);
  });

  it("renders row information and localized unknown size, speed, and ETA", async () => {
    await renderWithSettledTimers([
      task({
        id: "known",
        filename: "alpha.mp4",
        url: "https://cdn.example.com/files/alpha.mp4",
        state: "downloading",
        totalBytes: 200,
        downloadedBytes: 50,
        connections: 8,
        createdAtMs: 2,
      }),
      task({
        id: "unknown",
        filename: "mystery.bin",
        url: "https://mirror.example.net/x",
        state: "downloading",
        totalBytes: null,
        downloadedBytes: 0,
        connections: 4,
        createdAtMs: 1,
      }),
    ]);

    const known = screen
      .getByText("alpha.mp4")
      .closest('[data-testid="task-row"]') as HTMLElement | null;
    expect(known).not.toBeNull();
    if (!known) {
      throw new Error("known task row was not rendered");
    }
    expect(within(known).getByText(/downloading/i)).toBeInTheDocument();
    expect(within(known).getByText("cdn.example.com")).toBeInTheDocument();
    expect(within(known).getByText(en["nav.video"])).toBeInTheDocument();
    expect(known).toHaveTextContent(/\b8\b/);
    expect(known).toHaveTextContent(/200\s?B/i);
    expect(known).toHaveTextContent(/25\s?%/i);
    const knownProgress = within(known).getByRole("progressbar");
    expect(Number(knownProgress.getAttribute("aria-valuenow"))).toBe(25);

    const unknown = screen
      .getByText("mystery.bin")
      .closest('[data-testid="task-row"]') as HTMLElement | null;
    expect(unknown).not.toBeNull();
    if (!unknown) {
      throw new Error("unknown task row was not rendered");
    }
    expect(within(unknown).getByText("mirror.example.net")).toBeInTheDocument();
    expect(within(unknown).getByText(en["nav.other"])).toBeInTheDocument();
    expect(
      within(unknown).getAllByText(en["common.unknown"]).length,
    ).toBeGreaterThanOrEqual(3);
    expect(unknown).not.toHaveTextContent("0 B/s");
    expect(unknown).not.toHaveTextContent("0%");
    expect(
      within(unknown).getByRole("progressbar"),
    ).not.toHaveAttribute("aria-valuenow");
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

    expect(
      await screen.findByText(en["error.runtimeUnavailable"]),
    ).toBeInTheDocument();
  });

  it("applies the theme from settings to the document root", async () => {
    mockIpc([], { theme: "dark" });
    renderShell();

    await screen.findByText(en["list.emptyTitle"]);

    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("renders Chinese dictionary strings when settings select zh-CN", async () => {
    mockIpc([], { language: "zh-CN" });
    renderShell();

    expect(await screen.findByText(zh["list.emptyTitle"])).toBeInTheDocument();
    expect(screen.getByText(zh["nav.all"])).toBeInTheDocument();
    expect(screen.getByText(zh["details.empty"])).toBeInTheDocument();
  });
});

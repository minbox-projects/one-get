import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../i18n";
import type {
  AddDownloadRequest,
  DownloadSegment,
  DownloadSummary,
  ProbeResult,
} from "../types/downloads";
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
    if (command === "settings_status") {
      return {
        ok: true as const,
        data: { readOnly: false, recovered: false },
      };
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
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
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

function dialogSubmitButton(container: HTMLElement): HTMLButtonElement {
  const buttons = within(container).getAllByRole("button");
  const match = buttons.find((button) => {
    const label = `${button.textContent ?? ""} ${button.getAttribute("aria-label") ?? ""}`;
    return (
      /^(start|add|create|begin|download)\b/i.test(label.trim()) &&
      !/header|cancel|close|new download|batch|advanced/i.test(label)
    );
  });

  if (!match) {
    throw new Error("The dialog submit button was not found.");
  }

  return match as HTMLButtonElement;
}

async function setDialogField(
  user: ReturnType<typeof userEvent.setup>,
  field: HTMLElement,
  value: string,
) {
  if (field.tagName === "SELECT") {
    await user.selectOptions(field, value);
    return;
  }

  await user.clear(field);
  if (value !== "") {
    await user.type(field, value);
  }
}

describe("AppShell download dialogs", () => {
  it("opens New download from the toolbar and refreshes the list after an add", async () => {
    const user = userEvent.setup();
    let downloads: DownloadSummary[] = [];
    let addedRequest: AddDownloadRequest | null = null;

    invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "settings_get") {
        return {
          ok: true as const,
          data: settings({ defaultDirectory: "/tmp/downloads" }),
        };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
      }
      if (command === "downloads_list") {
        return { ok: true as const, data: downloads };
      }
      if (command === "downloads_probe") {
        return {
          ok: true as const,
          data: {
            finalUrl: "https://example.com/added.bin",
            totalBytes: 1024,
            rangeSupported: true,
            filename: "added.bin",
            contentType: "application/octet-stream",
            etag: null,
            lastModified: null,
          } satisfies ProbeResult,
        };
      }
      if (command === "downloads_add") {
        addedRequest = (args as { request: AddDownloadRequest }).request;
        downloads = [
          task({
            id: "added",
            url: addedRequest.url,
            filename: addedRequest.filename,
            state: "queued",
            createdAtMs: 10,
          }),
        ];
        return { ok: true as const, data: "added" };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    renderShell();
    await screen.findByText(en["list.emptyTitle"]);
    const listCallsBefore = invokeCommand.mock.calls.filter(
      ([command]) => command === "downloads_list",
    ).length;

    await user.click(screen.getByRole("button", { name: en["toolbar.new"] }));
    const dialog = await screen.findByRole("dialog", { name: /new download/i });
    await user.type(
      within(dialog).getByLabelText(/url/i),
      "https://example.com/added.bin",
    );

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith(
        "downloads_probe",
        expect.anything(),
      );
    });
    const filename = within(dialog).getByLabelText(/filename/i);
    await waitFor(() => {
      expect(filename).toHaveValue("added.bin");
    });

    await user.click(dialogSubmitButton(dialog));

    await waitFor(() => {
      expect(addedRequest).not.toBeNull();
    });
    expect(addedRequest).toMatchObject({
      url: "https://example.com/added.bin",
      filename: "added.bin",
    });

    await waitFor(() => {
      const listCalls = invokeCommand.mock.calls.filter(
        ([command]) => command === "downloads_list",
      ).length;
      expect(listCalls).toBeGreaterThan(listCallsBefore);
    });
    await waitFor(() => {
      const rows = screen.getAllByTestId("task-row");
      expect(
        rows.some((row) => (row.textContent ?? "").includes("added.bin")),
      ).toBe(true);
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: /new download/i }),
      ).not.toBeInTheDocument();
    });
  });

  it("opens Batch add from the toolbar and adds a URL already present in the store", async () => {
    const user = userEvent.setup();
    let downloads: DownloadSummary[] = [
      task({
        id: "existing",
        url: "https://example.com/existing.bin",
        filename: "existing.bin",
      }),
    ];
    const added: AddDownloadRequest[] = [];

    invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "settings_get") {
        return {
          ok: true as const,
          data: settings({ defaultDirectory: "/tmp/downloads" }),
        };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
      }
      if (command === "downloads_list") {
        return { ok: true as const, data: downloads };
      }
      if (command === "downloads_add") {
        const request = (args as { request: AddDownloadRequest }).request;
        added.push(request);
        downloads = [
          ...downloads,
          task({
            id: `added-${added.length}`,
            url: request.url,
            filename: request.filename,
            state: "queued",
            createdAtMs: 100 + added.length,
          }),
        ];
        return { ok: true as const, data: `added-${added.length}` };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    renderShell();
    await screen.findByText("existing.bin");

    await user.click(screen.getByRole("button", { name: en["toolbar.batch"] }));
    const dialog = await screen.findByRole("dialog", { name: /batch/i });
    await user.type(
      within(dialog).getByLabelText(/urls|one url per line/i),
      "https://example.com/existing.bin",
    );
    const destination = within(dialog).queryByLabelText(/destination/i);
    if (destination) {
      await setDialogField(user, destination, "/tmp/downloads");
    }

    await user.click(dialogSubmitButton(dialog));

    await waitFor(() => {
      expect(added).toHaveLength(1);
    });
    expect(added[0].url).toBe("https://example.com/existing.bin");

    await waitFor(() => {
      const rows = screen
        .getAllByTestId("task-row")
        .filter((row) => (row.textContent ?? "").includes("existing.bin"));
      expect(rows.length).toBeGreaterThanOrEqual(2);
    });
  });
});

describe("AppShell task controls, details, and deletion", () => {
  afterEach(() => {
    delete (navigator as { clipboard?: unknown }).clipboard;
  });

  function commandIds(command: string): string[] {
    return invokeCommand.mock.calls
      .filter(([name]) => name === command)
      .map(([, args]) => (args as { id?: string } | undefined)?.id ?? "")
      .sort();
  }

  function selectedTask(overrides: Partial<DownloadSummary> = {}) {
    return task({
      id: "task-1",
      filename: "report.pdf",
      url: "https://example.com/files/report.pdf",
      destination: "/Users/me/Downloads",
      state: "completed",
      downloadedBytes: 2048,
      totalBytes: 2048,
      createdAtMs: 1,
      updatedAtMs: 2,
      completedAtMs: 2,
      ...overrides,
    });
  }

  const segments: DownloadSegment[] = [
    { index: 0, start: 0, end: 1023, downloadedBytes: 1023, state: "done" },
    { index: 1, start: 1024, end: 2047, downloadedBytes: 1024, state: "done" },
  ];

  function mockDetailsIpc(
    downloads: DownloadSummary[],
    overrides: Partial<Record<string, unknown>> = {},
  ) {
    invokeCommand.mockImplementation(async (command: string) => {
      if (command === "settings_get") {
        return { ok: true as const, data: settings() };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
      }
      if (command === "downloads_list") {
        return { ok: true as const, data: downloads };
      }
      if (command === "downloads_segments") {
        return { ok: true as const, data: segments };
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

  async function selectFirstRow(
    user: ReturnType<typeof userEvent.setup>,
    filename: string,
  ) {
    await screen.findByText(filename);
    await user.click(screen.getByText(filename));
  }

  async function findByTextOrValue(value: string) {
    await waitFor(() => {
      const matches =
        screen.queryAllByText(value).length +
        screen.queryAllByDisplayValue(value).length;
      expect(matches).toBeGreaterThan(0);
    });
  }

  it("loads segments and shows details when a row is selected, with working open actions", async () => {
    const user = userEvent.setup();
    const current = selectedTask();
    mockDetailsIpc([current]);
    renderShell();

    await selectFirstRow(user, "report.pdf");

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_segments", {
        id: "task-1",
      });
    });

    await findByTextOrValue("/Users/me/Downloads");
    await findByTextOrValue(current.url);
    expect(screen.queryByText(en["details.empty"])).not.toBeInTheDocument();
    expect(screen.getAllByText(/^done$/i).length).toBeGreaterThan(0);
    expect(document.body).not.toHaveTextContent(/token=/i);

    await user.click(screen.getByRole("button", { name: /open file/i }));
    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_open_file", {
        id: "task-1",
      });
    });

    await user.click(screen.getByRole("button", { name: /open folder/i }));
    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_open_folder", {
        id: "task-1",
      });
    });
  });

  it("calls the expected bulk commands for every eligible task and refreshes", async () => {
    const user = userEvent.setup();
    const downloads = [
      task({ id: "downloading", filename: "downloading.bin", state: "downloading" }),
      task({ id: "queued", filename: "queued.bin", state: "queued" }),
      task({ id: "connecting", filename: "connecting.bin", state: "connecting" }),
      task({ id: "finalizing", filename: "finalizing.bin", state: "finalizing" }),
      task({ id: "paused", filename: "paused.bin", state: "paused" }),
      task({ id: "completed", filename: "completed.bin", state: "completed" }),
    ];
    mockDetailsIpc(downloads);
    renderShell();

    await screen.findByText("downloading.bin");
    const listCallsBefore = invokeCommand.mock.calls.filter(
      ([command]) => command === "downloads_list",
    ).length;

    await user.click(
      screen.getByRole("button", { name: en["toolbar.pauseAll"] }),
    );

    await waitFor(() => {
      expect(commandIds("downloads_pause")).toEqual([
        "connecting",
        "downloading",
        "queued",
      ]);
    });
    expect(commandIds("downloads_pause")).not.toContain("finalizing");
    expect(commandIds("downloads_pause")).not.toContain("paused");
    expect(commandIds("downloads_pause")).not.toContain("completed");

    await waitFor(() => {
      const listCalls = invokeCommand.mock.calls.filter(
        ([command]) => command === "downloads_list",
      ).length;
      expect(listCalls).toBeGreaterThan(listCallsBefore);
    });

    await user.click(
      screen.getByRole("button", { name: en["toolbar.resumeAll"] }),
    );

    await waitFor(() => {
      expect(commandIds("downloads_resume")).toEqual(["paused"]);
    });
  });

  it("deletes a record only after the record confirmation is accepted", async () => {
    const user = userEvent.setup();
    mockDetailsIpc([selectedTask()]);
    renderShell();

    await selectFirstRow(user, "report.pdf");
    await user.click(screen.getByRole("button", { name: /delete record/i }));

    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent(
      /file.*(keep|stays|remain|preserv)|(keep|preserve).*file/i,
    );

    await user.click(
      within(dialog).getByRole("button", { name: /delete record/i }),
    );

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_delete_record", {
        id: "task-1",
      });
    });
    expect(commandIds("downloads_delete_file")).toEqual([]);
  });

  it("requires the file-deletion acknowledgement before calling the delete-file command", async () => {
    const user = userEvent.setup();
    mockDetailsIpc([selectedTask()]);
    renderShell();

    await selectFirstRow(user, "report.pdf");
    await user.click(screen.getByRole("button", { name: /delete file/i }));

    const dialog = await screen.findByRole("dialog");
    const confirm = within(dialog).getByRole("button", {
      name: /delete file/i,
    });
    expect(confirm).toBeDisabled();

    await user.click(within(dialog).getByRole("checkbox"));
    expect(confirm).toBeEnabled();
    await user.click(confirm);

    await waitFor(() => {
      expect(invokeCommand).toHaveBeenCalledWith("downloads_delete_file", {
        id: "task-1",
      });
    });
    expect(commandIds("downloads_delete_record")).toEqual([]);
  });

  it("copies the stored redacted URL and reports clipboard failures", async () => {
    const user = userEvent.setup();
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const current = selectedTask();
    mockDetailsIpc([current]);
    renderShell();

    await selectFirstRow(user, "report.pdf");
    await user.click(screen.getByRole("button", { name: /copy (url|link)/i }));

    await waitFor(() => {
      expect(writeText).toHaveBeenCalledWith(current.url);
    });
    expect(document.body).not.toHaveTextContent(/token=/i);

    writeText.mockRejectedValueOnce(new Error("Clipboard permission denied."));
    await user.click(screen.getByRole("button", { name: /copy (url|link)/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/clipboard|copy/i);
  });

  it("shows a typed error from an open action without crashing", async () => {
    const user = userEvent.setup();
    mockDetailsIpc([selectedTask()], {
      downloads_open_file: {
        ok: false as const,
        error: { code: "file_missing", message: "The file is missing." },
      },
    });
    renderShell();

    await selectFirstRow(user, "report.pdf");
    await user.click(screen.getByRole("button", { name: /open file/i }));

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/missing|could not|open/i);
    expect(screen.getByText("report.pdf")).toBeInTheDocument();
  });
});

describe("AppShell settings", () => {
  it("opens settings from the toolbar, applies saved values live, and gives new downloads the saved defaults", async () => {
    const user = userEvent.setup();
    let current = settings({ defaultConnections: 4 });
    let saved: Record<string, unknown> | null = null;

    invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "settings_get") {
        return { ok: true as const, data: current };
      }
      if (command === "settings_status") {
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
      }
      if (command === "settings_save") {
        saved = (args as { settings: Record<string, unknown> }).settings;
        current = saved as unknown as typeof current;
        return { ok: true as const, data: current };
      }
      if (command === "downloads_list") {
        return { ok: true as const, data: [] };
      }
      return {
        ok: false as const,
        error: { code: "unexpected_command", message: command },
      };
    });

    renderShell();
    await screen.findByText(en["list.emptyTitle"]);

    await user.click(
      screen.getByRole("button", { name: en["toolbar.settings"] }),
    );
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByLabelText(/language/i);

    await user.selectOptions(
      within(dialog).getByLabelText(/language/i),
      "zh-CN",
    );
    await user.selectOptions(within(dialog).getByLabelText(/theme/i), "dark");
    const destination = within(dialog).getByLabelText(
      /default (download )?directory/i,
    );
    await user.clear(destination);
    await user.type(destination, "/tmp/saved");
    const connections = within(dialog).getByLabelText(/default connections/i);
    await user.clear(connections);
    await user.type(connections, "2");

    await user.click(within(dialog).getByRole("button", { name: /^save/i }));

    await waitFor(() => {
      expect(saved).toMatchObject({
        schemaVersion: 2,
        language: "zh-CN",
        theme: "dark",
        defaultDirectory: "/tmp/saved",
        defaultConnections: 2,
      });
    });
    await waitFor(() => {
      expect(document.documentElement.dataset.theme).toBe("dark");
    });
    expect(
      await screen.findByRole("button", { name: zh["toolbar.new"] }),
    ).toBeInTheDocument();

    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    });

    await user.click(screen.getByRole("button", { name: zh["toolbar.new"] }));
    const newDialog = await screen.findByRole("dialog", {
      name: zh["download.newTitle"],
    });
    expect(
      within(newDialog).getByLabelText(zh["download.destination"]),
    ).toHaveValue("/tmp/saved");
    expect(
      within(newDialog).getByLabelText(zh["download.connections"]),
    ).toHaveValue(2);
  });
});

import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../i18n";
import type { DownloadSummary } from "../types/downloads";

import { AppShell } from "./AppShell";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

const en = dictionaries["en-US"];

function settings() {
  return {
    schemaVersion: 2,
    language: "en-US",
    theme: "system",
    closeBehavior: "tray",
    defaultDirectory: "/tmp/downloads",
    defaultConnections: 8,
    maxConcurrentTasks: 3,
    globalRateLimit: null,
    logLevel: "info",
  };
}

function task(overrides: Partial<DownloadSummary> = {}): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/files/report.pdf",
    filename: "report.pdf",
    destination: "/tmp/downloads",
    state: "completed",
    downloadedBytes: 2048,
    totalBytes: 2048,
    connections: 1,
    createdAtMs: 2,
    updatedAtMs: 2,
    completedAtMs: 2,
    errorCode: null,
    errorMessage: null,
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

function mockIpc(initial: DownloadSummary[]) {
  let current = initial.map((entry) => ({ ...entry }));

  invokeCommand.mockImplementation(async (command: string, args?: unknown) => {
    const id = (args as { id?: string } | undefined)?.id ?? "";

    switch (command) {
      case "settings_get":
        return { ok: true as const, data: settings() };
      case "settings_status":
        return {
          ok: true as const,
          data: { readOnly: false, recovered: false },
        };
      case "downloads_list":
        return { ok: true as const, data: current.map((entry) => ({ ...entry })) };
      case "downloads_segments":
        return { ok: true as const, data: [] };
      case "downloads_pause":
        current = current.map((entry) =>
          entry.id === id
            ? ({ ...entry, state: "paused" } satisfies DownloadSummary)
            : entry,
        );
        return { ok: true as const, data: undefined };
      case "downloads_resume":
        current = current.map((entry) =>
          entry.id === id
            ? ({ ...entry, state: "downloading" } satisfies DownloadSummary)
            : entry,
        );
        return { ok: true as const, data: undefined };
      case "downloads_delete_record":
      case "downloads_delete_file":
        current = current.filter((entry) => entry.id !== id);
        return { ok: true as const, data: undefined };
      default:
        if (command.startsWith("downloads_")) {
          return { ok: true as const, data: undefined };
        }
        return {
          ok: false as const,
          error: { code: "unexpected_command", message: command },
        };
    }
  });

  return {
    commandIds(command: string): string[] {
      return invokeCommand.mock.calls
        .filter(([name]) => name === command)
        .map(([, args]) => (args as { id?: string } | undefined)?.id ?? "");
    },
  };
}

beforeEach(() => {
  invokeCommand.mockReset();
});

describe("AppShell keyboard shortcuts", () => {
  it("opens New download, Batch add, Settings and focuses search with modifier shortcuts", async () => {
    const user = userEvent.setup();
    mockIpc([]);
    renderShell();
    await screen.findByText(en["list.emptyTitle"]);

    await user.keyboard("{Control>}n{/Control}");
    expect(
      await screen.findByRole("dialog", { name: en["download.newTitle"] }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    });

    await user.keyboard("{Control>}{Shift>}n{/Shift}{/Control}");
    expect(
      await screen.findByRole("dialog", { name: en["batch.title"] }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    });

    await user.keyboard("{Control>},{/Control}");
    expect(
      await screen.findByRole("dialog", { name: en["settings.title"] }),
    ).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    });

    await user.keyboard("{Control>}f{/Control}");
    expect(screen.getByLabelText(en["toolbar.search"])).toHaveFocus();
  });

  it("ignores shortcuts while typing in a text field", async () => {
    const user = userEvent.setup();
    mockIpc([]);
    renderShell();
    await screen.findByText(en["list.emptyTitle"]);

    const search = screen.getByLabelText(en["toolbar.search"]);
    await user.click(search);
    await user.type(search, "report");

    await user.keyboard("{Control>}n{/Control}");
    await user.keyboard("{Control>},{/Control}");
    await user.keyboard("{Control>}{Shift>}n{/Shift}{/Control}");

    expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    expect(search).toHaveValue("report");
  });

  it("toggles pause and resume with Space on the selected task", async () => {
    const user = userEvent.setup();
    const ipc = mockIpc([
      task({ id: "active", filename: "alpha.mp4", state: "downloading" }),
    ]);
    renderShell();

    await screen.findByText("alpha.mp4");
    await user.click(screen.getByText("alpha.mp4"));

    await user.keyboard(" ");
    await waitFor(() => {
      expect(ipc.commandIds("downloads_pause")).toEqual(["active"]);
    });
    await waitFor(() => {
      expect(
        screen.getAllByRole("listitem")[0],
      ).toHaveTextContent(en["state.paused"]);
    });

    await user.keyboard(" ");
    await waitFor(() => {
      expect(ipc.commandIds("downloads_resume")).toEqual(["active"]);
    });
  });

  it("opens the delete-record confirmation with Delete and confirms it", async () => {
    const user = userEvent.setup();
    const ipc = mockIpc([task()]);
    renderShell();

    await screen.findByText("report.pdf");
    await user.click(screen.getByText("report.pdf"));
    await user.keyboard("{Delete}");

    const dialog = await screen.findByRole("dialog", {
      name: en["delete.recordTitle"],
    });
    expect(dialog).toHaveTextContent(en["delete.recordBody"]);
    expect(ipc.commandIds("downloads_delete_record")).toHaveLength(0);

    await user.click(
      within(dialog).getByRole("button", { name: en["delete.recordConfirm"] }),
    );
    await waitFor(() => {
      expect(ipc.commandIds("downloads_delete_record")).toEqual(["task-1"]);
    });
  });

  it("opens details with Enter on a focused row", async () => {
    const user = userEvent.setup();
    mockIpc([task()]);
    renderShell();

    await screen.findByText("report.pdf");
    const row = screen.getAllByRole("listitem")[0];
    row.focus();
    expect(row).toHaveFocus();

    await user.keyboard("{Enter}");

    expect(
      screen.getByRole("region", { name: en["details.label"] }),
    ).toHaveTextContent("https://example.com/files/report.pdf");
  });

  it("restores focus to the trigger after a dialog closes", async () => {
    const user = userEvent.setup();
    mockIpc([]);
    renderShell();
    await screen.findByText(en["list.emptyTitle"]);

    const trigger = screen.getByRole("button", { name: en["toolbar.new"] });
    await user.click(trigger);
    await screen.findByRole("dialog", { name: en["download.newTitle"] });

    await user.keyboard("{Escape}");
    await waitFor(() => {
      expect(screen.queryAllByRole("dialog")).toHaveLength(0);
    });
    expect(trigger).toHaveFocus();
  });
});

describe("AppShell accessibility semantics", () => {
  it("exposes labelled regions, list selection state, focusable rows and icon-only names", async () => {
    const user = userEvent.setup();
    mockIpc([task()]);
    renderShell();

    await screen.findByText("report.pdf");
    await user.click(screen.getByText("report.pdf"));

    expect(
      screen.getByRole("navigation", { name: en["nav.label"] }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: en["list.label"] }),
    ).toBeInTheDocument();

    const list = screen.getByRole("list", { name: en["list.label"] });
    const row = within(list).getAllByRole("listitem")[0];
    expect(row).toHaveAttribute("aria-selected", "true");
    expect(row).toHaveAttribute("tabindex", "0");

    expect(
      screen.getByRole("region", { name: en["details.label"] }),
    ).toBeInTheDocument();
    expect(screen.getByRole("region", { name: /status/i })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: en["toolbar.new"] }));
    const dialog = await screen.findByRole("dialog", {
      name: en["download.newTitle"],
    });
    expect(
      within(dialog).getByRole("button", { name: en["common.close"] }),
    ).toBeInTheDocument();
  });
});

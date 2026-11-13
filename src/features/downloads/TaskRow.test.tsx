import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type { DownloadState, DownloadSummary } from "../../types/downloads";

import { TaskRow } from "./TaskRow";

const en = dictionaries["en-US"];

function task(
  state: DownloadState,
  overrides: Partial<DownloadSummary> = {},
): DownloadSummary {
  return {
    id: `task-${state}`,
    url: "https://cdn.example.com/files/file.bin",
    filename: "file.bin",
    destination: "/tmp/downloads",
    state,
    downloadedBytes: 50,
    totalBytes: 200,
    connections: 8,
    createdAtMs: 1,
    updatedAtMs: 1,
    completedAtMs: state === "completed" ? 2 : null,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

function renderRow(
  state: DownloadState,
  overrides: Partial<DownloadSummary> = {},
  handlers: {
    onSelect?: (task: DownloadSummary) => void;
    onPause?: (task: DownloadSummary) => void;
    onResume?: (task: DownloadSummary) => void;
    onCancel?: (task: DownloadSummary) => void;
    onRetry?: (task: DownloadSummary) => void;
    selected?: boolean;
  } = {},
) {
  const current = task(state, overrides);
  const view = render(
    <I18nProvider language="en-US">
      <TaskRow task={current} speed={null} {...handlers} />
    </I18nProvider>,
  );
  return { ...view, current };
}

function button(name: RegExp): HTMLElement {
  return screen.getByRole("button", { name });
}

describe("TaskRow controls", () => {
  it.each([
    ["queued", { pause: true, resume: false, cancel: true, retry: false }],
    ["connecting", { pause: true, resume: false, cancel: true, retry: false }],
    ["downloading", { pause: true, resume: false, cancel: true, retry: false }],
    ["paused", { pause: false, resume: true, cancel: true, retry: false }],
    ["finalizing", { pause: false, resume: false, cancel: true, retry: false }],
    ["completed", { pause: false, resume: false, cancel: false, retry: false }],
    ["failed", { pause: false, resume: false, cancel: true, retry: true }],
    ["cancelled", { pause: false, resume: false, cancel: false, retry: false }],
  ] as const)(
    "enables only the actions available for a %s task",
    (state, expected) => {
      renderRow(state);
      expect(button(/pause/i)).toHaveProperty("disabled", !expected.pause);
      expect(button(/resume/i)).toHaveProperty("disabled", !expected.resume);
      expect(button(/cancel/i)).toHaveProperty("disabled", !expected.cancel);
      expect(button(/retry/i)).toHaveProperty("disabled", !expected.retry);
    },
  );

  it("calls the matching handler with the row task", async () => {
    const user = userEvent.setup();
    const onPause = vi.fn();
    const { current } = renderRow("downloading", {}, { onPause });

    await user.click(button(/pause/i));
    expect(onPause).toHaveBeenCalledTimes(1);
    expect(onPause).toHaveBeenCalledWith(current);
  });

  it("resumes a paused task with the row task", async () => {
    const user = userEvent.setup();
    const onResume = vi.fn();
    const { current } = renderRow("paused", {}, { onResume });

    await user.click(button(/resume/i));
    expect(onResume).toHaveBeenCalledWith(current);
  });

  it("cancels an active task with the row task", async () => {
    const user = userEvent.setup();
    const onCancel = vi.fn();
    const { current } = renderRow("downloading", {}, { onCancel });

    await user.click(button(/cancel/i));
    expect(onCancel).toHaveBeenCalledWith(current);
  });

  it("retries a failed task with the row task", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const { current } = renderRow("failed", {}, { onRetry });

    await user.click(button(/retry/i));
    expect(onRetry).toHaveBeenCalledWith(current);
  });

  it("selects the row for details and marks the selected row", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    const { container, current } = renderRow(
      "downloading",
      {},
      { onSelect, selected: true },
    );

    await user.click(screen.getByText("file.bin"));
    expect(onSelect).toHaveBeenCalledWith(current);
    expect(
      container.querySelector('[aria-selected="true"], [data-selected="true"]'),
    ).not.toBeNull();
  });

  it("keeps the existing row information for known values", () => {
    renderRow("downloading", {
      filename: "clip.mp4",
      url: "https://cdn.example.com/files/clip.mp4",
      totalBytes: 200,
      downloadedBytes: 50,
      connections: 8,
    });

    expect(screen.getByText("clip.mp4")).toBeInTheDocument();
    expect(screen.getByText("cdn.example.com")).toBeInTheDocument();
    expect(screen.getByText(en["nav.video"])).toBeInTheDocument();
    expect(screen.getByText(/downloading/i)).toBeInTheDocument();
    expect(screen.getByText(/200\s?B/i)).toBeInTheDocument();
    expect(screen.getByText(/25\s?%/i)).toBeInTheDocument();
    expect(screen.getByRole("progressbar")).toHaveAttribute(
      "aria-valuenow",
      "25",
    );
  });
});

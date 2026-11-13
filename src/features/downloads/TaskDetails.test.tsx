import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi, type Mock } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import { formatDate } from "../../lib/format";
import type {
  DownloadSegment,
  DownloadState,
  DownloadSummary,
} from "../../types/downloads";

import { TaskDetails } from "./TaskDetails";

const en = dictionaries["en-US"];

function task(
  state: DownloadState = "completed",
  overrides: Partial<DownloadSummary> = {},
): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/files/report.pdf",
    filename: "report.pdf",
    destination: "/Users/me/Downloads",
    state,
    downloadedBytes: 2048,
    totalBytes: 2048,
    connections: 8,
    createdAtMs: Date.UTC(2026, 10, 13, 1, 2),
    updatedAtMs: Date.UTC(2026, 10, 13, 2, 3),
    completedAtMs: Date.UTC(2026, 10, 13, 2, 3),
    errorCode: "CHECKSUM_MISMATCH",
    errorMessage: "The file hash did not match the expected value.",
    ...overrides,
  };
}

const segments: DownloadSegment[] = [
  {
    index: 0,
    start: 0,
    end: 1023,
    downloadedBytes: 1023,
    state: "done",
  },
  {
    index: 1,
    start: 1024,
    end: 2047,
    downloadedBytes: 1024,
    state: "failed",
  },
];

interface Handlers {
  onOpenFile?: Mock<(task: DownloadSummary | string) => void>;
  onOpenFolder?: Mock<(task: DownloadSummary | string) => void>;
  onCopyUrl?: Mock<(task: DownloadSummary | string) => void>;
  onClose?: Mock<() => void>;
  segmentsLoading?: boolean;
  segmentsError?: { code: string; message: string } | null;
  speed?: number | null;
}

function renderDetails(
  current: DownloadSummary = task(),
  handlers: Handlers = {},
  segmentList: DownloadSegment[] = segments,
) {
  const onOpenFile =
    handlers.onOpenFile ?? vi.fn<(task: DownloadSummary | string) => void>();
  const onOpenFolder =
    handlers.onOpenFolder ?? vi.fn<(task: DownloadSummary | string) => void>();
  const onCopyUrl =
    handlers.onCopyUrl ?? vi.fn<(task: DownloadSummary | string) => void>();
  const onClose = handlers.onClose ?? vi.fn<() => void>();
  const view = render(
    <I18nProvider language="en-US">
      <TaskDetails
        task={current}
        segments={segmentList}
        segmentsLoading={handlers.segmentsLoading ?? false}
        segmentsError={handlers.segmentsError ?? null}
        speed={handlers.speed === undefined ? 1024 : handlers.speed}
        onOpenFile={onOpenFile}
        onOpenFolder={onOpenFolder}
        onCopyUrl={onCopyUrl}
        onClose={onClose}
      />
    </I18nProvider>,
  );
  return { ...view, onOpenFile, onOpenFolder, onCopyUrl, onClose, current };
}

function expectTextOrValue(value: string): void {
  const byText = screen.queryAllByText(value);
  const byValue = screen.queryAllByDisplayValue(value);
  expect(byText.length + byValue.length).toBeGreaterThan(0);
}

describe("TaskDetails", () => {
  it("shows the stored redacted URL, directory, state, sizes, timestamps, error, and segments", () => {
    renderDetails();

    expectTextOrValue("https://example.com/files/report.pdf");
    expectTextOrValue("/Users/me/Downloads");
    expect(screen.getAllByText(/completed/i).length).toBeGreaterThan(0);
    expect(document.body).toHaveTextContent(/2\s?KB/i);
    expect(document.body).toHaveTextContent(/1\s?KB\/s/i);
    expect(document.body).toHaveTextContent(/0\s?sec/i);
    expect(document.body).toHaveTextContent(/\b8\b/);
    expectTextOrValue(formatDate(Date.UTC(2026, 10, 13, 1, 2), "en-US") ?? "");
    expectTextOrValue(
      formatDate(Date.UTC(2026, 10, 13, 2, 3), "en-US") ?? "",
    );
    expect(screen.getByText("CHECKSUM_MISMATCH")).toBeInTheDocument();
    expect(en["error.CHECKSUM_MISMATCH"]).toBeTruthy();
    expect(screen.getByText(en["error.CHECKSUM_MISMATCH"])).toBeInTheDocument();

    expect(screen.getAllByText(/^done$/i).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/^failed$/i).length).toBeGreaterThan(0);
    expect(document.body).toHaveTextContent("1,023");
    expect(document.body).toHaveTextContent("1,024");
    expect(document.body).not.toHaveTextContent(/token=/i);
  });

  it("renders unknown speed and ETA instead of fabricated values", () => {
    renderDetails(
      task("downloading", {
        downloadedBytes: 0,
        totalBytes: null,
        completedAtMs: null,
        errorCode: null,
        errorMessage: null,
      }),
      { speed: null },
    );

    expect(screen.getAllByText(en["common.unknown"]).length).toBeGreaterThanOrEqual(2);
    expect(document.body).not.toHaveTextContent("0 B/s");
    expect(document.body).not.toHaveTextContent("0%");
  });

  it("shows segment loading and segment error states", () => {
    const { rerender } = renderDetails();
    rerender(
      <I18nProvider language="en-US">
        <TaskDetails
          task={task()}
          segments={[]}
          segmentsLoading
          segmentsError={null}
          speed={null}
          onOpenFile={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyUrl={vi.fn()}
          onClose={vi.fn()}
        />
      </I18nProvider>,
    );
    expect(screen.getByText(/loading/i)).toBeInTheDocument();

    rerender(
      <I18nProvider language="en-US">
        <TaskDetails
          task={task()}
          segments={[]}
          segmentsLoading={false}
          segmentsError={{
            code: "store_error",
            message: "Segments could not be loaded.",
          }}
          speed={null}
          onOpenFile={vi.fn()}
          onOpenFolder={vi.fn()}
          onCopyUrl={vi.fn()}
          onClose={vi.fn()}
        />
      </I18nProvider>,
    );
    expect(
      screen.getByText("Segments could not be loaded."),
    ).toBeInTheDocument();
  });

  it("invokes open-file, open-folder, copy-URL, and close actions", async () => {
    const user = userEvent.setup();
    const { onOpenFile, onOpenFolder, onCopyUrl, onClose, current } =
      renderDetails();

    await user.click(screen.getByRole("button", { name: /open file/i }));
    expect(onOpenFile).toHaveBeenCalledTimes(1);
    const openFileArg = onOpenFile.mock.calls[0][0];
    expect(openFileArg === current || openFileArg === current.id).toBe(true);

    await user.click(screen.getByRole("button", { name: /open folder/i }));
    expect(onOpenFolder).toHaveBeenCalledTimes(1);
    const openFolderArg = onOpenFolder.mock.calls[0][0];
    expect(openFolderArg === current || openFolderArg === current.id).toBe(true);

    await user.click(screen.getByRole("button", { name: /copy (url|link)/i }));
    expect(onCopyUrl).toHaveBeenCalledTimes(1);
    const copyArg = onCopyUrl.mock.calls[0][0];
    expect(copyArg === current || copyArg === current.url).toBe(true);

    await user.click(screen.getByRole("button", { name: /close/i }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not offer open-file or open-folder when the task has no destination", () => {
    renderDetails(task("failed", { destination: null }));

    const openFile = screen.queryByRole("button", { name: /open file/i });
    const openFolder = screen.queryByRole("button", { name: /open folder/i });
    expect(openFile === null || (openFile as HTMLButtonElement).disabled).toBe(
      true,
    );
    expect(
      openFolder === null || (openFolder as HTMLButtonElement).disabled,
    ).toBe(true);
  });

  it("treats segment end as exclusive and renders a one-byte segment as complete", () => {
    renderDetails(
      task("completed", {
        downloadedBytes: 1,
        totalBytes: 1,
        completedAtMs: 1,
        errorCode: null,
        errorMessage: null,
      }),
      {},
      [{ index: 0, start: 0, end: 1, downloadedBytes: 1, state: "done" }],
    );

    const row = screen.getByText("Segment 0").closest("li");
    expect(row).not.toBeNull();
    expect(row).toHaveTextContent("1 / 1");
    const progress = within(row as HTMLElement).getByRole("progressbar");
    expect(progress).toHaveAttribute("aria-valuenow", "100");
    expect(progress).not.toHaveAttribute("aria-valuenow", "50");
  });

  it.each(["en-US", "zh-CN"] as const)(
    "shows the localized actionable CHECKSUM_MISMATCH message and code in %s",
    (language) => {
      const dictionary = dictionaries[language];
      expect(dictionary["error.CHECKSUM_MISMATCH"]).toBeTruthy();
      expect(dictionary["error.CHECKSUM_MISMATCH"]).toMatch(/SHA-256/i);
      expect(dictionary["error.CHECKSUM_MISMATCH"]).toMatch(/retry|重试/i);

      const raw = "The file hash did not match the expected value.";
      const { unmount } = render(
        <I18nProvider language={language}>
          <TaskDetails
            task={task("failed", {
              errorCode: "CHECKSUM_MISMATCH",
              errorMessage: raw,
            })}
            segments={[]}
            speed={null}
            onOpenFile={vi.fn()}
            onOpenFolder={vi.fn()}
            onCopyUrl={vi.fn()}
            onClose={vi.fn()}
          />
        </I18nProvider>,
      );

      expect(screen.getByText("CHECKSUM_MISMATCH")).toBeInTheDocument();
      expect(
        screen.getByText(dictionary["error.CHECKSUM_MISMATCH"]),
      ).toBeInTheDocument();
      expect(screen.queryByText(raw)).not.toBeInTheDocument();
      unmount();
    },
  );

  it.each(["en-US", "zh-CN"] as const)(
    "maps INVALID_STATE to localized text instead of the raw engine message in %s",
    (language) => {
      const dictionary = dictionaries[language];
      expect(dictionary["error.INVALID_STATE"]).toBeTruthy();
      const raw = "The task is not in a state that allows this operation.";
      const { unmount } = render(
        <I18nProvider language={language}>
          <TaskDetails
            task={task("failed", {
              errorCode: "INVALID_STATE",
              errorMessage: raw,
            })}
            segments={[]}
            speed={null}
            onOpenFile={vi.fn()}
            onOpenFolder={vi.fn()}
            onCopyUrl={vi.fn()}
            onClose={vi.fn()}
          />
        </I18nProvider>,
      );

      expect(screen.getByText("INVALID_STATE")).toBeInTheDocument();
      expect(
        screen.getByText(dictionary["error.INVALID_STATE"]),
      ).toBeInTheDocument();
      expect(screen.queryByText(raw)).not.toBeInTheDocument();
      unmount();
    },
  );
});

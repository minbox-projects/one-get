import { act, renderHook } from "@testing-library/react";
import type { Mock } from "vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DownloadSummary } from "../../types/downloads";

import { useDownloads } from "./useDownloads";

const downloadsApi = vi.hoisted(() => ({
  listDownloads: vi.fn(),
  getDownloadSegments: vi.fn(),
  probeDownload: vi.fn(),
  addDownload: vi.fn(),
  pauseDownload: vi.fn(),
  resumeDownload: vi.fn(),
  cancelDownload: vi.fn(),
  retryDownload: vi.fn(),
  deleteDownloadRecord: vi.fn(),
  deleteDownloadFile: vi.fn(),
  openDownloadFile: vi.fn(),
  openDownloadFolder: vi.fn(),
}));

vi.mock("../../lib/downloads", () => downloadsApi);

function task(
  id: string,
  state: DownloadSummary["state"],
  overrides: Partial<DownloadSummary> = {},
): DownloadSummary {
  return {
    id,
    url: "https://example.com/file.bin",
    filename: `${id}.bin`,
    destination: "/tmp/downloads",
    state,
    downloadedBytes: 0,
    totalBytes: 100,
    connections: 8,
    createdAtMs: 1,
    updatedAtMs: 1,
    completedAtMs: state === "completed" ? 2 : null,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

const ok = { ok: true as const, data: undefined };
const failed = {
  ok: false as const,
  error: { code: "invalid_state", message: "Task cannot be paused." },
};

function listCalls(): number {
  return downloadsApi.listDownloads.mock.calls.length;
}

function idsOf(mock: Mock): string[] {
  return mock.mock.calls.map(([id]) => id as string).sort();
}

async function flush(ms = 0): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

function setVisibility(state: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", {
    configurable: true,
    value: state,
  });
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  setVisibility("visible");
  for (const mock of Object.values(downloadsApi)) {
    mock.mockReset();
  }
  downloadsApi.listDownloads.mockResolvedValue({
    ok: true,
    data: [task("task-1", "downloading")],
  });
  downloadsApi.pauseDownload.mockResolvedValue(ok);
  downloadsApi.resumeDownload.mockResolvedValue(ok);
  downloadsApi.cancelDownload.mockResolvedValue(ok);
  downloadsApi.retryDownload.mockResolvedValue(ok);
  downloadsApi.deleteDownloadRecord.mockResolvedValue(ok);
  downloadsApi.deleteDownloadFile.mockResolvedValue(ok);
});

afterEach(() => {
  vi.useRealTimers();
  delete (document as { visibilityState?: string }).visibilityState;
});

describe("useDownloads actions", () => {
  it("calls every per-task wrapper with the id and refreshes after each action", async () => {
    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    const cases: Array<[string, Mock]> = [
      ["pauseTask", downloadsApi.pauseDownload],
      ["resumeTask", downloadsApi.resumeDownload],
      ["cancelTask", downloadsApi.cancelDownload],
      ["retryTask", downloadsApi.retryDownload],
      ["deleteRecord", downloadsApi.deleteDownloadRecord],
      ["deleteFile", downloadsApi.deleteDownloadFile],
    ];

    for (const [method, wrapper] of cases) {
      const before = listCalls();
      const action = (
        result.current as unknown as Record<
          string,
          (id: string) => Promise<void>
        >
      )[method];

      await act(async () => {
        await action("task-1");
      });

      expect(wrapper).toHaveBeenCalledWith("task-1");
      expect(listCalls()).toBe(before + 1);
    }
  });

  it("surfaces a typed action error without dropping the current list", async () => {
    downloadsApi.pauseDownload.mockResolvedValue(failed);

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    await act(async () => {
      await result.current.pauseTask("task-1");
    });

    expect(result.current.actionError).toEqual(failed.error);
    expect(result.current.tasks).toHaveLength(1);
    expect(result.current.tasks[0].id).toBe("task-1");

    act(() => {
      result.current.clearActionError();
    });
    expect(result.current.actionError).toBeNull();
  });

  it("applies bulk pause and resume only to eligible tasks", async () => {
    downloadsApi.listDownloads.mockResolvedValue({
      ok: true,
      data: [
        task("downloading", "downloading"),
        task("queued", "queued"),
        task("connecting", "connecting"),
        task("finalizing", "finalizing"),
        task("paused", "paused"),
        task("failed", "failed"),
        task("completed", "completed"),
        task("cancelled", "cancelled"),
      ],
    });

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    await act(async () => {
      await result.current.pauseAll();
    });

    expect(idsOf(downloadsApi.pauseDownload)).toEqual([
      "connecting",
      "downloading",
      "queued",
    ]);
    expect(downloadsApi.resumeDownload).not.toHaveBeenCalled();

    await act(async () => {
      await result.current.resumeAll();
    });

    expect(idsOf(downloadsApi.resumeDownload)).toEqual(["paused"]);
  });
});

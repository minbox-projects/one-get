import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DownloadSummary } from "../../types/downloads";
import { useDownloads } from "./useDownloads";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({
  invokeCommand,
  isTauriRuntime: () => true,
}));

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

function listCalls(): number {
  return invokeCommand.mock.calls.filter(([command]) => command === "downloads_list")
    .length;
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
  invokeCommand.mockReset();
  vi.useFakeTimers();
  setVisibility("visible");
});

afterEach(() => {
  vi.useRealTimers();
  delete (document as { visibilityState?: string }).visibilityState;
});

describe("useDownloads", () => {
  it("loads tasks and exposes view, search, and sort controls", async () => {
    const initial = task();
    invokeCommand.mockResolvedValue({ ok: true, data: [initial] });

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    expect(result.current.loading).toBe(false);
    expect(result.current.error).toBeFalsy();
    expect(result.current.tasks).toEqual([initial]);

    act(() => {
      result.current.setView("video");
      result.current.setSearch("clip");
      result.current.setSortKey("size");
      result.current.setSortDirection("asc");
    });

    expect(result.current.view).toBe("video");
    expect(result.current.search).toBe("clip");
    expect(result.current.sortKey).toBe("size");
    expect(result.current.sortDirection).toBe("asc");
  });

  it("polls about once per second while visible and pauses while hidden", async () => {
    invokeCommand.mockResolvedValue({ ok: true, data: [task()] });

    renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    expect(listCalls()).toBe(1);

    await flush(3000);
    const visiblePolls = listCalls();
    expect(visiblePolls).toBeGreaterThanOrEqual(4);
    expect(visiblePolls).toBeLessThanOrEqual(5);

    setVisibility("hidden");
    const hiddenAt = listCalls();
    await flush(3000);
    expect(listCalls()).toBe(hiddenAt);

    setVisibility("visible");
    await flush(1000);
    expect(listCalls()).toBeGreaterThanOrEqual(hiddenAt + 1);
  });

  it("fetches immediately when refresh is called, even while hidden", async () => {
    invokeCommand.mockResolvedValue({ ok: true, data: [task()] });

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    setVisibility("hidden");
    const before = listCalls();

    await act(async () => {
      await result.current.refresh();
    });

    expect(listCalls()).toBe(before + 1);
  });

  it("surfaces an error state when a poll fails", async () => {
    invokeCommand.mockResolvedValue({
      ok: false,
      error: { code: "runtime_unavailable", message: "runtime missing" },
    });

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    expect(result.current.loading).toBe(false);
    expect(result.current.tasks).toEqual([]);
    expect(result.current.error).toBeTruthy();
  });

  it("derives speed from successive samples and reports null when unknown", async () => {
    const initial = task({ downloadedBytes: 0 });
    let sample = [initial];
    invokeCommand.mockImplementation(async (command: string) =>
      command === "downloads_list"
        ? { ok: true, data: sample }
        : { ok: false, error: { code: "unexpected_command", message: command } },
    );

    const { result } = renderHook(() => useDownloads({ pollIntervalMs: 1000 }));
    await flush();

    expect(result.current.speedOf(task({ id: "never-seen" }))).toBeNull();

    sample = [{ ...initial, downloadedBytes: 2048 }];
    await flush(1000);

    const speed = result.current.speedOf(result.current.tasks[0]);
    expect(speed).not.toBeNull();
    expect(speed as number).toBeGreaterThan(0);
    expect(speed as number).toBeLessThanOrEqual(4096);
  });
});

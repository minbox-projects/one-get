import { describe, expect, it } from "vitest";

import type { DownloadState, DownloadSummary } from "../../types/downloads";

import { taskActions } from "./controls";

function task(
  state: DownloadState,
  overrides: Partial<DownloadSummary> = {},
): DownloadSummary {
  return {
    id: `task-${state}`,
    url: "https://example.com/file.bin",
    filename: "file.bin",
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

describe("taskActions", () => {
  it.each([
    [
      "queued",
      {
        pause: true,
        resume: false,
        cancel: true,
        retry: false,
        deleteRecord: false,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "connecting",
      {
        pause: true,
        resume: false,
        cancel: true,
        retry: false,
        deleteRecord: false,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "downloading",
      {
        pause: true,
        resume: false,
        cancel: true,
        retry: false,
        deleteRecord: false,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "paused",
      {
        pause: false,
        resume: true,
        cancel: true,
        retry: false,
        deleteRecord: false,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "finalizing",
      {
        pause: false,
        resume: false,
        cancel: true,
        retry: false,
        deleteRecord: false,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "completed",
      {
        pause: false,
        resume: false,
        cancel: false,
        retry: false,
        deleteRecord: true,
        deleteFile: true,
        openFile: true,
        openFolder: true,
      },
    ],
    [
      "failed",
      {
        pause: false,
        resume: false,
        cancel: true,
        retry: true,
        deleteRecord: true,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
    [
      "cancelled",
      {
        pause: false,
        resume: false,
        cancel: false,
        retry: false,
        deleteRecord: true,
        deleteFile: false,
        openFile: false,
        openFolder: true,
      },
    ],
  ] as const)("matches the availability matrix for %s tasks", (state, expected) => {
    expect(taskActions(task(state))).toEqual(expected);
  });

  it.each([
    "queued",
    "connecting",
    "downloading",
    "paused",
    "finalizing",
    "completed",
    "failed",
    "cancelled",
  ] as const)(
    "exposes open-folder only when a %s task has a destination",
    (state) => {
      expect(taskActions(task(state, { destination: null })).openFolder).toBe(
        false,
      );
      expect(taskActions(task(state, { destination: "/tmp/out" })).openFolder).toBe(
        true,
      );
    },
  );

  it("keeps record and file deletion unavailable for active tasks", () => {
    for (const state of [
      "queued",
      "connecting",
      "downloading",
      "paused",
      "finalizing",
    ] as const) {
      const actions = taskActions(task(state));
      expect(actions.deleteRecord).toBe(false);
      expect(actions.deleteFile).toBe(false);
    }
  });
});

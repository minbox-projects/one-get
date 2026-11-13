import type { DownloadState, DownloadSummary } from "../../types/downloads";

export interface TaskActionAvailability {
  pause: boolean;
  resume: boolean;
  cancel: boolean;
  retry: boolean;
  deleteRecord: boolean;
  deleteFile: boolean;
  openFile: boolean;
  openFolder: boolean;
}

const PAUSABLE: ReadonlySet<DownloadState> = new Set([
  "queued",
  "connecting",
  "downloading",
]);

const RESUMABLE: ReadonlySet<DownloadState> = new Set(["paused"]);

const CANCELLABLE: ReadonlySet<DownloadState> = new Set([
  "queued",
  "connecting",
  "downloading",
  "paused",
  "failed",
  "finalizing",
]);

const RETRYABLE: ReadonlySet<DownloadState> = new Set(["failed"]);

const RECORD_DELETABLE: ReadonlySet<DownloadState> = new Set([
  "completed",
  "failed",
  "cancelled",
]);

/**
 * Resolves which actions a task's current state permits. An active task must be
 * cancelled before its record or file can be deleted, and a file only exists
 * for completed tasks; the folder action needs a recorded destination.
 */
export function taskActions(task: DownloadSummary): TaskActionAvailability {
  const completed = task.state === "completed";

  return {
    pause: PAUSABLE.has(task.state),
    resume: RESUMABLE.has(task.state),
    cancel: CANCELLABLE.has(task.state),
    retry: RETRYABLE.has(task.state),
    deleteRecord: RECORD_DELETABLE.has(task.state),
    deleteFile: completed,
    openFile: completed,
    openFolder: task.destination !== null,
  };
}

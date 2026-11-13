import type { IpcResult } from "../types/app";
import type {
  AddDownloadRequest,
  DownloadSegment,
  DownloadSummary,
  ProbeDownloadRequest,
  ProbeResult,
} from "../types/downloads";

import { invokeCommand } from "./tauri";

/**
 * Returns the persistence-safe form of a task URL: userinfo, query and fragment
 * are dropped so a token intentionally kept out of storage is never displayed
 * or copied.
 */
export function redactStoredUrl(raw: string): string {
  try {
    const url = new URL(raw);
    url.username = "";
    url.password = "";
    url.search = "";
    url.hash = "";
    return url.toString();
  } catch {
    return raw
      .replace(/^([a-z][a-z0-9+.-]*:\/\/)[^@/]+@/i, "$1")
      .replace(/[?#].*$/, "");
  }
}

/** Lists the lightweight download summaries used by the task list. */
export function listDownloads(): Promise<IpcResult<DownloadSummary[]>> {
  return invokeCommand<DownloadSummary[]>("downloads_list");
}

/** Loads the segment plan for one download. */
export function getDownloadSegments(
  id: string,
): Promise<IpcResult<DownloadSegment[]>> {
  return invokeCommand<DownloadSegment[]>("downloads_segments", { id });
}

/** Probes a remote URL for filename, size and range support. */
export function probeDownload(
  request: ProbeDownloadRequest,
): Promise<IpcResult<ProbeResult>> {
  return invokeCommand<ProbeResult>("downloads_probe", { request });
}

/** Creates one queued download task and returns its identifier. */
export function addDownload(
  request: AddDownloadRequest,
): Promise<IpcResult<string>> {
  return invokeCommand<string>("downloads_add", { request });
}

export function pauseDownload(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_pause", { id });
}

export function resumeDownload(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_resume", { id });
}

export function cancelDownload(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_cancel", { id });
}

export function retryDownload(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_retry", { id });
}

export function deleteDownloadRecord(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_delete_record", { id });
}

export function deleteDownloadFile(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_delete_file", { id });
}

export function openDownloadFile(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_open_file", { id });
}

export function openDownloadFolder(id: string): Promise<IpcResult<void>> {
  return invokeCommand<void>("downloads_open_folder", { id });
}

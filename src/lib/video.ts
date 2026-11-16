import type { IpcResult } from "../types/app";
import type {
  VideoProbeRequest,
  VideoProbeResult,
  VideoStartRequest,
} from "../types/video";

import { invokeCommand } from "./tauri";

/** Start request that may carry the dialog's connection preference. */
export interface VideoStartDownloadRequest extends VideoStartRequest {
  connections?: number;
}

/** Probes a video URL through the verified component tools. */
export function probeVideo(
  request: VideoProbeRequest,
): Promise<IpcResult<VideoProbeResult>> {
  return invokeCommand<VideoProbeResult>("video_probe", { request });
}

/** Persists one video task with the selected quality and network options. */
export function startVideoDownload(
  request: VideoStartDownloadRequest,
): Promise<IpcResult<string>> {
  return invokeCommand<string>("video_start", { request });
}

import type { IpcResult } from "../types/app";
import type {
  VideoProbeRequest,
  VideoProbeResult,
  VideoStartRequest,
} from "../types/video";

import { invokeCommand } from "./tauri";

/** Probes a video URL through the verified component tools. */
export function probeVideo(
  request: VideoProbeRequest,
): Promise<IpcResult<VideoProbeResult>> {
  return invokeCommand<VideoProbeResult>("video_probe", { request });
}

/** Persists one video task with the selected quality and network options. */
export function startVideoDownload(
  request: VideoStartRequest,
): Promise<IpcResult<string>> {
  return invokeCommand<string>("video_start", { request });
}

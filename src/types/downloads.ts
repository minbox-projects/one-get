export type DownloadState =
  | "queued"
  | "connecting"
  | "downloading"
  | "paused"
  | "finalizing"
  | "completed"
  | "failed"
  | "cancelled";

export type DownloadCategory =
  | "video"
  | "model"
  | "document"
  | "program"
  | "other";

export type SegmentState = "pending" | "active" | "done" | "failed";

export type DownloadKind = "http" | "video";

/** Persisted video metadata exposed alongside a video task summary. */
export interface VideoSummary {
  title: string | null;
  durationSecs: number | null;
  uploader: string | null;
  quality: string | null;
  stage: string | null;
  audioOnly: boolean;
}

export interface DownloadSummary {
  id: string;
  kind?: DownloadKind;
  video?: VideoSummary | null;
  url: string;
  finalUrl?: string | null;
  filename: string;
  destination: string | null;
  state: DownloadState;
  downloadedBytes: number;
  totalBytes: number | null;
  connections: number;
  createdAtMs: number;
  updatedAtMs: number;
  completedAtMs: number | null;
  errorCode: string | null;
  errorMessage: string | null;
}

export interface DownloadSegment {
  index: number;
  start: number;
  end: number;
  downloadedBytes: number;
  state: SegmentState;
}

export type HeaderPair = [name: string, value: string];

export interface ProbeDownloadRequest {
  url: string;
  allowHtml: boolean;
  extraHeaders: HeaderPair[];
}

export interface ProbeResult {
  finalUrl: string;
  totalBytes: number | null;
  rangeSupported: boolean;
  filename: string;
  contentType: string | null;
  etag: string | null;
  lastModified: string | null;
}

export interface AddDownloadRequest {
  url: string;
  filename: string;
  destination: string | null;
  connections: number;
  rateLimit: number | null;
  expectedSha256: string | null;
  allowHtml: boolean;
  extraHeaders: HeaderPair[];
}

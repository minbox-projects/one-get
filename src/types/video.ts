/** One selectable video or audio-only quality choice returned by a probe. */
export interface VideoChoice {
  id: string;
  label: string;
  sizeBytes: number | null;
  mp4Compatible: boolean;
  formatSelector: string;
}

/** Inputs for one yt-dlp metadata probe. */
export interface VideoProbeRequest {
  url: string;
  cookiesFile: string | null;
  proxyUrl: string | null;
}

/** Probe metadata and ordered quality choices for the quality dialog. */
export interface VideoProbeResult {
  title: string;
  durationSecs: number | null;
  uploader: string | null;
  video: VideoChoice[];
  audio: VideoChoice[];
  defaultIndex: number | null;
}

/** Parsed probe metadata stored with a video task. */
export interface VideoStartMetadata {
  title: string;
  durationSecs?: number | null;
  uploader?: string | null;
  quality?: string;
}

/** Inputs for creating one video download task. */
export interface VideoStartRequest {
  url: string;
  filename: string;
  destination: string | null;
  formatSelector: string;
  audioOnly: boolean;
  metadata: VideoStartMetadata;
  cookiesFile: string | null;
  proxyUrl: string | null;
}

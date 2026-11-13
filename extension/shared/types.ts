export const RESOURCE_CATEGORIES = [
  "video",
  "audio",
  "archive",
  "program",
  "document",
  "model",
  "other",
] as const;

export type ResourceCategory = (typeof RESOURCE_CATEGORIES)[number];

export type ResourceKind = ResourceCategory | "hls" | "dash" | "video-platform";

export type CaptureIntent = "takeover" | "prefill";

export type CaptureKind = "file" | "hls" | "dash" | "video-platform";

export interface SafeCaptureHeader {
  name: string;
  value: string;
}

export interface CaptureDraftItem {
  url: string;
  filename?: string;
  referer?: string;
  mime?: string;
  size?: number | null;
  kind?: CaptureKind;
  headers?: SafeCaptureHeader[];
}

export interface Classification {
  kind: ResourceKind;
  category: ResourceCategory | null;
}

export interface ResourceInput {
  url: string;
  tabId: number;
  frameId: number;
  pageUrl: string;
  nowMs: number;
  filename?: string;
  mime?: string | null;
  size?: number | null;
}

export interface ResourceEntry {
  url: string;
  tabId: number;
  frameId: number;
  pageUrl: string;
  capturedAtMs: number;
  filename: string;
  mime: string | null;
  size: number | null;
  kind: ResourceKind;
  category: ResourceCategory | null;
}

export function isResourceCategory(value: unknown): value is ResourceCategory {
  return typeof value === "string" && (RESOURCE_CATEGORIES as readonly string[]).includes(value);
}

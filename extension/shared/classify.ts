import type { Classification, ResourceCategory } from "./types";
import { RESOURCE_CATEGORIES } from "./types";

export const CATEGORY_EXTENSIONS: Record<ResourceCategory, readonly string[]> = {
  video: ["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v"],
  audio: ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"],
  archive: ["zip", "7z", "rar", "tar", "gz", "bz2", "xz"],
  program: ["dmg", "exe", "msi", "pkg", "apk", "appimage", "deb", "rpm"],
  document: ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "epub", "csv"],
  model: ["safetensors", "gguf", "ckpt", "pt", "pth", "onnx"],
  other: [],
};

export const HLS_MIME_TYPES: readonly string[] = [
  "application/vnd.apple.mpegurl",
  "application/x-mpegurl",
  "audio/mpegurl",
  "audio/x-mpegurl",
];

export const DASH_MIME_TYPES: readonly string[] = ["application/dash+xml"];

const MIME_CATEGORIES: Record<string, ResourceCategory> = {
  "application/pdf": "document",
  "application/msword": "document",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "document",
  "application/vnd.ms-excel": "document",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "document",
  "application/vnd.ms-powerpoint": "document",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation": "document",
  "application/epub+zip": "document",
  "text/plain": "document",
  "text/markdown": "document",
  "text/csv": "document",
  "application/zip": "archive",
  "application/x-7z-compressed": "archive",
  "application/x-rar-compressed": "archive",
  "application/x-tar": "archive",
  "application/gzip": "archive",
  "application/x-gzip": "archive",
  "application/x-bzip2": "archive",
  "application/x-xz": "archive",
  "application/x-msdownload": "program",
  "application/vnd.microsoft.portable-executable": "program",
  "application/x-msi": "program",
  "application/x-apple-diskimage": "program",
  "application/vnd.android.package-archive": "program",
  "application/x-debian-package": "program",
  "application/x-rpm": "program",
  "application/x-safetensors": "model",
  "application/x-gguf": "model",
  "application/x-pytorch": "model",
  "application/x-onnx": "model",
};

const SEGMENT_EXTENSIONS = [".ts", ".m4s", ".m4f", ".cmfv", ".cmfa"];

const PLAYBACK_SEGMENT_KEYS = [
  "range",
  "sq",
  "rn",
  "rbuf",
  "itag",
  "bytestart",
  "byteend",
  "frag",
  "segment",
  "seg",
  "part",
];

const EXTENSION_CATEGORIES = new Map<string, ResourceCategory>();
for (const category of RESOURCE_CATEGORIES) {
  for (const extension of CATEGORY_EXTENSIONS[category]) {
    EXTENSION_CATEGORIES.set(extension, category);
  }
}

function decodeSegment(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function pathnameOf(url: string): string | null {
  try {
    return new URL(url).pathname;
  } catch {
    return null;
  }
}

function normaliseMime(mime: string | null | undefined): string | null {
  if (typeof mime !== "string") {
    return null;
  }
  const value = mime.split(";")[0].trim().toLowerCase();
  return value.length > 0 ? value : null;
}

function extensionOf(name: string): string | null {
  const lastDot = name.lastIndexOf(".");
  if (lastDot <= 0 || lastDot === name.length - 1) {
    return null;
  }
  return name.slice(lastDot + 1).toLowerCase();
}

function filenameOf(url: string): string | null {
  const pathname = pathnameOf(url);
  if (pathname === null) {
    return null;
  }
  const segments = pathname.split("/").filter((segment) => segment.length > 0);
  const last = segments[segments.length - 1];
  return last ? decodeSegment(last) : null;
}

export function isHlsManifest(url: string, mime?: string | null): boolean {
  const pathname = pathnameOf(url);
  if (pathname !== null && pathname.toLowerCase().endsWith(".m3u8")) {
    return true;
  }
  const normalizedMime = normaliseMime(mime);
  return normalizedMime !== null && HLS_MIME_TYPES.includes(normalizedMime);
}

export function isDashManifest(url: string, mime?: string | null): boolean {
  const pathname = pathnameOf(url);
  if (pathname !== null && pathname.toLowerCase().endsWith(".mpd")) {
    return true;
  }
  const normalizedMime = normaliseMime(mime);
  return normalizedMime !== null && DASH_MIME_TYPES.includes(normalizedMime);
}

export function isSegmentUrl(url: string): boolean {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }

  const pathname = parsed.pathname.toLowerCase();
  if (SEGMENT_EXTENSIONS.some((extension) => pathname.endsWith(extension))) {
    return true;
  }

  const segments = pathname.split("/");
  if (segments[segments.length - 1] === "init.mp4") {
    return true;
  }

  if (pathname.includes("videoplayback")) {
    return PLAYBACK_SEGMENT_KEYS.some((key) => parsed.searchParams.has(key));
  }

  return false;
}

export function classifyResource(input: {
  url: string;
  mime?: string | null;
  filename?: string | null;
}): Classification {
  if (isHlsManifest(input.url, input.mime)) {
    return { kind: "hls", category: null };
  }
  if (isDashManifest(input.url, input.mime)) {
    return { kind: "dash", category: null };
  }

  const name =
    typeof input.filename === "string" && input.filename.length > 0
      ? (input.filename.split(/[\\/]/).pop() ?? input.filename)
      : filenameOf(input.url);
  const extension = name === null ? null : extensionOf(name);
  if (extension !== null) {
    const category = EXTENSION_CATEGORIES.get(extension);
    if (category !== undefined) {
      return { kind: category, category };
    }
  }

  const mime = normaliseMime(input.mime);
  if (mime !== null) {
    const explicit = MIME_CATEGORIES[mime];
    if (explicit !== undefined) {
      return { kind: explicit, category: explicit };
    }
    if (mime.startsWith("video/")) {
      return { kind: "video", category: "video" };
    }
    if (mime.startsWith("audio/")) {
      return { kind: "audio", category: "audio" };
    }
  }

  return { kind: "other", category: "other" };
}

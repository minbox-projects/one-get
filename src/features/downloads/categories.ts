import type { DownloadCategory } from "../../types/downloads";

const CATEGORY_EXTENSIONS: Record<
  Exclude<DownloadCategory, "other">,
  readonly string[]
> = {
  video: ["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "ts"],
  model: ["safetensors", "gguf", "ckpt", "pt", "pth", "onnx"],
  document: [
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "txt",
    "md",
    "epub",
    "csv",
  ],
  program: [
    "dmg",
    "exe",
    "msi",
    "pkg",
    "zip",
    "7z",
    "rar",
    "tar",
    "gz",
    "apk",
    "appimage",
  ],
};

const EXTENSION_TO_CATEGORY = new Map<string, DownloadCategory>();
for (const [category, extensions] of Object.entries(CATEGORY_EXTENSIONS) as Array<
  [DownloadCategory, readonly string[]]
>) {
  for (const extension of extensions) {
    EXTENSION_TO_CATEGORY.set(extension, category);
  }
}

/** Derives the task category from the final filename extension. */
export function categoryForFilename(filename: string): DownloadCategory {
  const separator = filename.lastIndexOf(".");
  if (separator < 0 || separator === filename.length - 1) {
    return "other";
  }

  const extension = filename.slice(separator + 1).toLowerCase();
  return EXTENSION_TO_CATEGORY.get(extension) ?? "other";
}

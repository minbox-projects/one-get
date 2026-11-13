import { describe, expect, it } from "vitest";

import type { DownloadCategory } from "../../types/downloads";
import { categoryForFilename } from "./categories";

// REQ-001: categories come from the final filename extension, case-insensitively.
const EXTENSIONS: Array<[DownloadCategory, string[]]> = [
  ["video", ["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "ts"]],
  ["model", ["safetensors", "gguf", "ckpt", "pt", "pth", "onnx"]],
  [
    "document",
    [
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
  ],
  [
    "program",
    [
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
  ],
];

describe("categoryForFilename", () => {
  for (const [category, extensions] of EXTENSIONS) {
    it(`maps every ${category} extension in lower and upper case`, () => {
      for (const extension of extensions) {
        expect(categoryForFilename(`archive.${extension}`)).toBe(category);
        expect(categoryForFilename(`ARCHIVE.${extension.toUpperCase()}`)).toBe(
          category,
        );
        expect(categoryForFilename(`multi.part.Name.${extension}`)).toBe(
          category,
        );
      }
    });
  }

  it("maps unknown, absent, and trailing-dot extensions to other", () => {
    expect(categoryForFilename("payload.xyz")).toBe("other");
    expect(categoryForFilename("README")).toBe("other");
    expect(categoryForFilename("file.")).toBe("other");
    expect(categoryForFilename("")).toBe("other");
  });

  it("uses only the final extension", () => {
    expect(categoryForFilename("backup.tar.gz")).toBe("program");
    expect(categoryForFilename("movie.2026.MP4")).toBe("video");
    expect(categoryForFilename("notes.final.pdf")).toBe("document");
  });
});

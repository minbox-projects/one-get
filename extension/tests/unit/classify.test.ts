import { describe, expect, it } from "vitest";

import {
  CATEGORY_EXTENSIONS,
  DASH_MIME_TYPES,
  HLS_MIME_TYPES,
  classifyResource,
  isDashManifest,
  isHlsManifest,
  isSegmentUrl,
} from "../../shared/classify";
import { sanitizeFilename } from "../../shared/filename";
import type { ResourceCategory } from "../../shared/types";

const DOCUMENTED_EXTENSIONS: Record<ResourceCategory, string[]> = {
  video: ["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v"],
  audio: ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"],
  archive: ["zip", "7z", "rar", "tar", "gz", "bz2", "xz"],
  program: ["dmg", "exe", "msi", "pkg", "apk", "appimage", "deb", "rpm"],
  document: ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "epub", "csv"],
  model: ["safetensors", "gguf", "ckpt", "pt", "pth", "onnx"],
  other: [],
};

const categories = Object.keys(DOCUMENTED_EXTENSIONS) as ResourceCategory[];

describe("resource classification", () => {
  it("publishes exactly the documented category extension vocabulary", () => {
    for (const category of categories) {
      expect([...CATEGORY_EXTENSIONS[category]].sort()).toEqual(
        DOCUMENTED_EXTENSIONS[category].slice().sort(),
      );
    }
  });

  it("publishes the documented HLS and DASH manifest MIME types", () => {
    expect(new Set(HLS_MIME_TYPES)).toEqual(
      new Set([
        "application/vnd.apple.mpegurl",
        "application/x-mpegurl",
        "audio/mpegurl",
        "audio/x-mpegurl",
      ]),
    );
    expect(new Set(DASH_MIME_TYPES)).toEqual(new Set(["application/dash+xml"]));
  });

  it("recognizes HLS manifests by path or MIME without matching a query token", () => {
    expect(isHlsManifest("https://cdn.test/live/master.M3U8?token=abc")).toBe(true);
    expect(isHlsManifest("https://cdn.test/video.mp4", "application/vnd.apple.mpegurl")).toBe(true);
    expect(isHlsManifest("https://cdn.test/video.mp4", "Audio/X-MpegURL")).toBe(true);
    expect(isHlsManifest("https://cdn.test/video.mp4", "video/mp4")).toBe(false);
    expect(isHlsManifest("https://cdn.test/notes.txt?next=master.m3u8")).toBe(false);
  });

  it("recognizes DASH manifests by path or MIME", () => {
    expect(isDashManifest("https://cdn.test/stream/manifest.mpd?token=abc")).toBe(true);
    expect(isDashManifest("https://cdn.test/stream", "application/dash+xml")).toBe(true);
    expect(isDashManifest("https://cdn.test/manifest.m3u8", "application/vnd.apple.mpegurl")).toBe(
      false,
    );
  });

  it("filters transport and media segments without rejecting ordinary files", () => {
    for (const url of [
      "https://cdn.test/seg/1.ts?token=abc",
      "https://cdn.test/seg/2.m4s",
      "https://cdn.test/seg/3.m4f",
      "https://cdn.test/seg/4.cmfv",
      "https://cdn.test/seg/5.cmfa",
      "https://cdn.test/seg/init.mp4?token=abc",
      "https://r1.googlevideo.com/videoplayback?id=1&itag=137",
      "https://r1.googlevideo.com/videoplayback?id=1&part=3",
    ]) {
      expect(isSegmentUrl(url), url).toBe(true);
    }

    expect(isSegmentUrl("https://cdn.test/movie.mp4")).toBe(false);
    expect(isSegmentUrl("https://cdn.test/videoplayback?id=plain")).toBe(false);
    expect(isSegmentUrl("https://cdn.test/files?next=segment.ts")).toBe(false);
    expect(isSegmentUrl("https://cdn.test/movie.mp4?label=clip.ts")).toBe(false);
  });

  it("classifies manifests before extensions and MIME types", () => {
    expect(classifyResource({ url: "https://cdn.test/master.m3u8", mime: "video/mp4" })).toEqual({
      kind: "hls",
      category: null,
    });
    expect(classifyResource({ url: "https://cdn.test/master.mpd" })).toEqual({
      kind: "dash",
      category: null,
    });
  });

  it("prefers the lowercased filename extension over a conflicting MIME type", () => {
    expect(classifyResource({ url: "https://cdn.test/archive.ZIP", mime: "text/plain" })).toEqual({
      kind: "archive",
      category: "archive",
    });
    expect(classifyResource({ url: "https://cdn.test/model.GGUF", mime: "video/mp4" })).toEqual({
      kind: "model",
      category: "model",
    });
    expect(classifyResource({ url: "https://cdn.test/report.pdf" })).toEqual({
      kind: "document",
      category: "document",
    });
  });

  it("falls back to MIME and then other, including octet-stream", () => {
    expect(classifyResource({ url: "https://cdn.test/file", mime: "video/mp4" })).toEqual({
      kind: "video",
      category: "video",
    });
    expect(classifyResource({ url: "https://cdn.test/track", mime: "audio/ogg" })).toEqual({
      kind: "audio",
      category: "audio",
    });
    expect(
      classifyResource({ url: "https://cdn.test/package.7z", mime: "application/x-7z-compressed" }),
    ).toEqual({ kind: "archive", category: "archive" });
    expect(classifyResource({ url: "https://cdn.test/blob", mime: "application/octet-stream" })).toEqual(
      { kind: "other", category: "other" },
    );
    expect(classifyResource({ url: "https://cdn.test/seg.ts", mime: "video/mp2t" })).toEqual({
      kind: "video",
      category: "video",
    });
    expect(classifyResource({ url: "https://cdn.test/unknown.weird" })).toEqual({
      kind: "other",
      category: "other",
    });
  });
});

describe("filename byte bound", () => {
  // A2: display filenames stay within the 255-byte UTF-8 capture limit.
  it.each([
    { name: "three-byte", character: "汉" },
    { name: "four-byte", character: "😀" },
  ])(
    "truncates repeated $name characters without splitting a code point",
    ({ character }) => {
      const input = character.repeat(200);
      expect([...input].length).toBeLessThanOrEqual(255);
      expect(new TextEncoder().encode(input).length).toBeGreaterThan(255);

      const sanitized = sanitizeFilename(input);

      expect(sanitized.length).toBeGreaterThan(0);
      expect(new TextEncoder().encode(sanitized).length).toBeLessThanOrEqual(255);
      expect([...sanitized].every((value) => value === character)).toBe(true);
    },
  );
});

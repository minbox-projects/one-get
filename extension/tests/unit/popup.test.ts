import { describe, expect, it } from "vitest";

import {
  languageFromNavigator,
  pairingView,
  resourceView,
  takeoverFailureView,
} from "../../popup/popup";
import type { TakeoverFailure } from "../../chromium/background";
import { translateExtension } from "../../shared/i18n";
import type { ResourceEntry, ResourceKind } from "../../shared/types";

const TOKEN_SHAPED = "0123456789abcdef".repeat(4);
const LANGUAGES = ["zh-CN", "en-US"] as const;

const FILENAMES: Record<ResourceKind, string> = {
  video: "movie.mp4",
  audio: "track.mp3",
  archive: "bundle.zip",
  program: "installer.dmg",
  document: "report.pdf",
  model: "model.gguf",
  other: "file.bin",
  hls: "master.m3u8",
  dash: "manifest.mpd",
  "video-platform": "video",
};

const CATEGORY_KINDS: ResourceKind[] = [
  "video",
  "audio",
  "archive",
  "program",
  "document",
  "model",
  "other",
];

function resource(kind: ResourceKind, overrides: Partial<ResourceEntry> = {}): ResourceEntry {
  const filename = overrides.filename ?? FILENAMES[kind];
  return {
    url: `https://user:password@cdn.test/files/${filename}?token=${TOKEN_SHAPED}#fragment`,
    tabId: 1,
    frameId: 0,
    pageUrl: "https://page.test/watch",
    capturedAtMs: 1_000,
    filename,
    mime: null,
    size: 2 * 1024 * 1024,
    kind,
    category: kind === "hls" || kind === "dash" || kind === "video-platform" ? null : kind,
    ...overrides,
  };
}

function redactedUrl(kind: ResourceKind): string {
  return `https://cdn.test/files/${FILENAMES[kind]}`;
}

describe("popup resource rows", () => {
  it("shows every category with a localized kind, filename, known size, and send action", () => {
    for (const kind of CATEGORY_KINDS) {
      for (const language of LANGUAGES) {
        const view = resourceView(resource(kind), language);

        expect(view.filename).toBe(FILENAMES[kind]);
        expect(view.kind).toBe(translateExtension(language, `popup.kind.${kind}`));
        expect(view.size.length).toBeGreaterThan(0);
        expect(view.size).not.toBe(translateExtension(language, "popup.unknownSize"));
        expect(view.canSend).toBe(true);
        expect(view.url).toBe(redactedUrl(kind));
        expect(view.url).not.toContain("user");
        expect(view.url).not.toContain("password");
        expect(view.url).not.toContain("token=");
        expect(view.url).not.toContain("#");
        expect(JSON.stringify(view)).not.toMatch(/[0-9a-f]{64}/i);
      }
    }
  });

  it("shows the localized unknown size for a null size", () => {
    for (const language of LANGUAGES) {
      const view = resourceView(resource("video", { size: null }), language);

      expect(view.size).toBe(translateExtension(language, "popup.unknownSize"));
    }
  });

  it("keeps HLS and DASH rows visible with a disabled localized send explanation", () => {
    for (const kind of ["hls", "dash"] as const) {
      for (const language of LANGUAGES) {
        const view = resourceView(resource(kind), language);

        expect(view.filename).toBe(FILENAMES[kind]);
        expect(view.kind).toBe(translateExtension(language, `popup.kind.${kind}`));
        expect(view.size.length).toBeGreaterThan(0);
        expect(view.canSend).toBe(false);
        expect(view.sendDisabledReason).toBe(
          translateExtension(language, "popup.manifestUnavailable"),
        );
        expect(view.url).toBe(redactedUrl(kind));
        expect(JSON.stringify(view)).not.toMatch(/[0-9a-f]{64}/i);
      }
    }
  });

  it("localizes row labels in Simplified Chinese and English", () => {
    for (const kind of [...CATEGORY_KINDS, "hls", "dash"] as ResourceKind[]) {
      const zh = resourceView(resource(kind), "zh-CN");
      const en = resourceView(resource(kind), "en-US");

      expect(zh.kind).toBe(translateExtension("zh-CN", `popup.kind.${kind}`));
      expect(en.kind).toBe(translateExtension("en-US", `popup.kind.${kind}`));
      expect(zh.kind).not.toBe(en.kind);
    }
  });
});

describe("popup pairing view", () => {
  it("localizes paired and unpaired labels in both languages", () => {
    for (const paired of [true, false]) {
      const zh = pairingView(paired, "zh-CN");
      const en = pairingView(paired, "en-US");

      expect(zh.paired).toBe(paired);
      expect(en.paired).toBe(paired);
      expect(zh.status.length).toBeGreaterThan(0);
      expect(zh.action.length).toBeGreaterThan(0);
      expect(en.status.length).toBeGreaterThan(0);
      expect(en.action.length).toBeGreaterThan(0);
      expect(zh.status).not.toBe(en.status);
      expect(zh.action).not.toBe(en.action);
      expect(JSON.stringify(zh)).not.toMatch(/[0-9a-f]{64}/i);
      expect(JSON.stringify(en)).not.toMatch(/[0-9a-f]{64}/i);
    }
  });
});

describe("popup language selection", () => {
  it("follows Chinese and English navigator languages and falls back to English", () => {
    expect(languageFromNavigator("zh-CN")).toBe("zh-CN");
    expect(languageFromNavigator("zh")).toBe("zh-CN");
    expect(languageFromNavigator("en-US")).toBe("en-US");
    expect(languageFromNavigator("en-GB")).toBe("en-US");
    expect(languageFromNavigator(null)).toBe("en-US");
    expect(languageFromNavigator(undefined)).toBe("en-US");
    expect(languageFromNavigator("fr-FR")).toBe("en-US");
  });
});

describe("popup takeover failure view", () => {
  // AC-016: a failed takeover offers a localized retry over a redacted URL.
  it("localizes each failure reason and redacts the display URL in both languages", () => {
    const reasons: TakeoverFailure["reason"][] = [
      "capture_failed",
      "token_revoked",
      "update_required",
    ];

    for (const reason of reasons) {
      const failure: TakeoverFailure = {
        downloadId: 7,
        url: `https://user:password@cdn.test/files/movie.mp4?token=${TOKEN_SHAPED}#${TOKEN_SHAPED}`,
        filename: "movie.mp4",
        reason,
      };

      const zh = takeoverFailureView(failure, "zh-CN");
      const en = takeoverFailureView(failure, "en-US");

      for (const view of [zh, en]) {
        expect(view.message.length).toBeGreaterThan(0);
        expect(view.action.length).toBeGreaterThan(0);
        expect(view.url).toBe("https://cdn.test/files/movie.mp4");
        expect(view.url).not.toContain("user");
        expect(view.url).not.toContain("password");
        expect(view.url).not.toContain("token=");
        expect(view.url).not.toContain("#");
        expect(JSON.stringify(view)).not.toContain(TOKEN_SHAPED);
        expect(JSON.stringify(view)).not.toMatch(/[0-9a-f]{64}/i);
      }

      expect(zh.message).not.toBe(en.message);
      expect(zh.action).not.toBe(en.action);
      expect(zh.action).toMatch(/retry|again|try|send|重试|再次|重新/i);
      expect(en.action).toMatch(/retry|again|try|send|重试|再次|重新/i);
    }
  });
});

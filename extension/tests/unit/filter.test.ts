import { describe, expect, it } from "vitest";

import { decideTakeover, hostOf, isBlacklistedHost } from "../../shared/filter";
import type { TakeoverCandidate, TakeoverRules } from "../../shared/filter";
import type { ResourceCategory } from "../../shared/types";

function allCategories(value: boolean): Record<ResourceCategory, boolean> {
  return {
    video: value,
    audio: value,
    archive: value,
    program: value,
    document: value,
    model: value,
    other: value,
  };
}

function rules(overrides: Partial<TakeoverRules> = {}): TakeoverRules {
  return {
    takeoverEnabled: true,
    categories: allCategories(true),
    minimumSizeBytes: 1024,
    blacklistHosts: [],
    ...overrides,
  };
}

function candidate(overrides: Partial<TakeoverCandidate> = {}): TakeoverCandidate {
  return {
    url: "https://cdn.test/files/movie.mp4",
    kind: "video",
    category: "video",
    size: 4096,
    ...overrides,
  };
}

describe("takeover host matching", () => {
  it("matches blacklist entries exactly or as a dot-boundary subdomain", () => {
    const entries = ["Example.COM"];

    expect(isBlacklistedHost("example.com", entries)).toBe(true);
    expect(isBlacklistedHost("download.example.com", entries)).toBe(true);
    expect(isBlacklistedHost("deep.download.example.com", entries)).toBe(true);
    expect(isBlacklistedHost("badexample.com", entries)).toBe(false);
    expect(isBlacklistedHost("example.com.evil.test", entries)).toBe(false);
    expect(isBlacklistedHost("example.com", [])).toBe(false);
  });

  it("extracts a lowercase hostname and returns null for unparseable URLs", () => {
    expect(hostOf("https://User:Pass@Sub.Example.com:8443/path?q=1")).toBe("sub.example.com");
    expect(hostOf("http://example.com/")).toBe("example.com");
    expect(hostOf("not a url")).toBeNull();
  });
});

describe("takeover decisions", () => {
  it("rejects every candidate while takeover is disabled", () => {
    expect(decideTakeover(candidate(), rules({ takeoverEnabled: false }))).toEqual({
      takeOver: false,
      reason: "disabled",
    });
  });

  it("rejects non-http candidates before the browser download starts", () => {
    expect(
      decideTakeover(candidate({ url: "ftp://cdn.test/files/movie.mp4" }), rules()),
    ).toEqual({ takeOver: false, reason: "non_http" });
    expect(
      decideTakeover(candidate({ url: "data:video/mp4;base64,AAAA" }), rules()),
    ).toEqual({ takeOver: false, reason: "non_http" });
  });

  it("rejects a blacklisted host and its subdomains", () => {
    expect(
      decideTakeover(
        candidate({ url: "https://downloads.example.com/movie.mp4" }),
        rules({ blacklistHosts: ["example.com"] }),
      ),
    ).toEqual({ takeOver: false, reason: "blacklisted" });
  });

  it("rejects a disabled category while leaving other categories eligible", () => {
    const categories = allCategories(true);
    categories.video = false;
    const configured = rules({ categories });

    expect(decideTakeover(candidate(), configured)).toEqual({
      takeOver: false,
      reason: "category_disabled",
    });
    expect(
      decideTakeover(candidate({ url: "https://cdn.test/track.mp3", kind: "audio", category: "audio" }), configured),
    ).toEqual({ takeOver: true, reason: "eligible" });
  });

  it("applies the minimum size boundary and exempts an unknown size", () => {
    expect(decideTakeover(candidate({ size: 1023 }), rules())).toEqual({
      takeOver: false,
      reason: "too_small",
    });
    expect(decideTakeover(candidate({ size: 1024 }), rules())).toEqual({
      takeOver: true,
      reason: "eligible",
    });
    expect(decideTakeover(candidate({ size: 0 }), rules({ minimumSizeBytes: 0 }))).toEqual({
      takeOver: true,
      reason: "eligible",
    });
    expect(decideTakeover(candidate({ size: null }), rules())).toEqual({
      takeOver: true,
      reason: "eligible",
    });
  });

  it("never takes over HLS or DASH manifests, even with an unknown size", () => {
    for (const kind of ["hls", "dash"] as const) {
      expect(
        decideTakeover(candidate({ kind, category: null, size: null }), rules()),
      ).toEqual({ takeOver: false, reason: "manifest" });
    }
  });

  it("never takes over incognito or extension-originated downloads", () => {
    expect(decideTakeover(candidate({ incognito: true }), rules())).toEqual({
      takeOver: false,
      reason: "incognito",
    });
    expect(decideTakeover(candidate({ fromExtension: true }), rules())).toEqual({
      takeOver: false,
      reason: "extension_download",
    });
  });

  it("takes over an eligible media file of any enabled category", () => {
    expect(decideTakeover(candidate(), rules())).toEqual({ takeOver: true, reason: "eligible" });
    expect(
      decideTakeover(
        candidate({ url: "https://cdn.test/bundle.tar.gz", kind: "archive", category: "archive" }),
        rules(),
      ),
    ).toEqual({ takeOver: true, reason: "eligible" });
    expect(
      decideTakeover(
        candidate({ url: "https://cdn.test/book.epub", kind: "document", category: "document" }),
        rules(),
      ),
    ).toEqual({ takeOver: true, reason: "eligible" });
  });
});

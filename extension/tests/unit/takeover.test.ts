import { describe, expect, it } from "vitest";

import { decideTakeover } from "../../shared/filter";
import type { TakeoverCandidate } from "../../shared/filter";
import { DEFAULT_SETTINGS, normalizeSettings } from "../../shared/settings";
import type { ExtensionSettings } from "../../shared/settings";

function candidate(overrides: Partial<TakeoverCandidate> = {}): TakeoverCandidate {
  return {
    url: "https://cdn.test/files/movie.mp4",
    kind: "video",
    category: "video",
    size: 2 * 1024 * 1024,
    ...overrides,
  };
}

function saved(overrides: Partial<ExtensionSettings> = {}): ExtensionSettings {
  return normalizeSettings({ takeoverEnabled: true, ...overrides });
}

describe("takeover rules from saved settings", () => {
  it("uses the saved default category toggles, where media starts enabled", () => {
    expect(DEFAULT_SETTINGS.categories.video).toBe(true);
    expect(DEFAULT_SETTINGS.categories.audio).toBe(true);

    expect(decideTakeover(candidate(), saved())).toEqual({
      takeOver: true,
      reason: "eligible",
    });
    expect(
      decideTakeover(candidate({ url: "https://cdn.test/track.mp3", kind: "audio", category: "audio" }), saved()),
    ).toEqual({ takeOver: true, reason: "eligible" });
  });

  it("stays disabled until a saved document explicitly enables takeover", () => {
    expect(decideTakeover(candidate(), normalizeSettings({}))).toEqual({
      takeOver: false,
      reason: "disabled",
    });
  });

  it("applies a saved category toggle to the next download", () => {
    const categories = { ...DEFAULT_SETTINGS.categories, video: false };

    expect(decideTakeover(candidate(), saved({ categories }))).toEqual({
      takeOver: false,
      reason: "category_disabled",
    });
  });

  it("matches a saved blacklist host and its subdomains", () => {
    const rules = saved({ blacklistHosts: ["https://blocked.test/path"] });

    expect(
      decideTakeover(candidate({ url: "https://cdn.blocked.test/movie.mp4" }), rules),
    ).toEqual({ takeOver: false, reason: "blacklisted" });
    expect(
      decideTakeover(candidate({ url: "https://notblocked.test/movie.mp4" }), rules),
    ).toEqual({ takeOver: true, reason: "eligible" });
  });

  it("applies the saved minimum size while exempting HLS, DASH, and unknown sizes", () => {
    const rules = saved();

    expect(decideTakeover(candidate({ size: DEFAULT_SETTINGS.minimumSizeBytes }), rules)).toEqual({
      takeOver: true,
      reason: "eligible",
    });
    expect(
      decideTakeover(candidate({ size: DEFAULT_SETTINGS.minimumSizeBytes - 1 }), rules),
    ).toEqual({ takeOver: false, reason: "too_small" });
    expect(decideTakeover(candidate({ size: null }), rules)).toEqual({
      takeOver: true,
      reason: "eligible",
    });
    for (const kind of ["hls", "dash"] as const) {
      expect(decideTakeover(candidate({ kind, category: null, size: 1 }), rules)).toEqual({
        takeOver: false,
        reason: "manifest",
      });
    }
  });

  it("never captures incognito, extension-originated, or non-http downloads", () => {
    const rules = saved();

    expect(decideTakeover(candidate({ incognito: true }), rules)).toEqual({
      takeOver: false,
      reason: "incognito",
    });
    expect(decideTakeover(candidate({ fromExtension: true }), rules)).toEqual({
      takeOver: false,
      reason: "extension_download",
    });
    expect(decideTakeover(candidate({ url: "ftp://cdn.test/movie.mp4" }), rules)).toEqual({
      takeOver: false,
      reason: "non_http",
    });
  });

  it("applies a settings change to the next decision without a restart", () => {
    const enabled = saved();
    const disabled = normalizeSettings({ ...enabled, takeoverEnabled: false });

    expect(decideTakeover(candidate(), enabled)).toEqual({ takeOver: true, reason: "eligible" });
    expect(decideTakeover(candidate(), disabled)).toEqual({
      takeOver: false,
      reason: "disabled",
    });
  });
});

import { describe, expect, it } from "vitest";

import {
  DEFAULT_SETTINGS,
  mergeSettingsDocument,
  normalizeSettings,
} from "../../shared/settings";
import type { ExtensionSettings } from "../../shared/settings";
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

function settings(overrides: Partial<ExtensionSettings> = {}): ExtensionSettings {
  return {
    takeoverEnabled: true,
    categories: allCategories(true),
    minimumSizeBytes: 2048,
    blacklistHosts: ["example.com"],
    sendCookies: false,
    ...overrides,
  };
}

describe("extension settings defaults and validation", () => {
  it("defaults to disabled takeover, send-cookies off, every category enabled, and a one MiB minimum", () => {
    expect(DEFAULT_SETTINGS).toEqual({
      takeoverEnabled: false,
      categories: allCategories(true),
      minimumSizeBytes: 1048576,
      blacklistHosts: [],
      sendCookies: false,
    });
  });

  it("returns defaults for absent or unusable documents without throwing", () => {
    for (const raw of [undefined, null, "garbage", 42, []]) {
      expect(() => normalizeSettings(raw)).not.toThrow();
      expect(normalizeSettings(raw)).toEqual(DEFAULT_SETTINGS);
    }
  });

  it("loads sendCookies as false when a stored document has only the older five fields", () => {
    const normalized = normalizeSettings({
      takeoverEnabled: true,
      categories: allCategories(true),
      minimumSizeBytes: 4096,
      blacklistHosts: ["old.test"],
    });

    expect(normalized.sendCookies).toBe(false);
    expect(normalized.takeoverEnabled).toBe(true);
    expect(normalized.minimumSizeBytes).toBe(4096);
    expect(normalized.blacklistHosts).toEqual(["old.test"]);
  });

  it("round-trips a stored sendCookies true value", () => {
    const normalized = normalizeSettings({
      takeoverEnabled: false,
      categories: allCategories(true),
      minimumSizeBytes: 1048576,
      blacklistHosts: [],
      sendCookies: true,
    });

    expect(normalized.sendCookies).toBe(true);
  });

  it("falls back invalid non-boolean sendCookies values to false", () => {
    expect(normalizeSettings({ sendCookies: "yes" }).sendCookies).toBe(false);
    expect(normalizeSettings({ sendCookies: 1 }).sendCookies).toBe(false);
    expect(normalizeSettings({ sendCookies: null }).sendCookies).toBe(false);
    expect(normalizeSettings({ sendCookies: {} }).sendCookies).toBe(false);
  });

  it("ignores unknown category keys and defaults missing categories to enabled", () => {
    const normalized = normalizeSettings({
      categories: { video: false, audio: false, bogus: false },
    });

    expect(Object.keys(normalized.categories).sort()).toEqual(
      ["archive", "audio", "document", "model", "other", "program", "video"].sort(),
    );
    expect(normalized.categories.video).toBe(false);
    expect(normalized.categories.audio).toBe(false);
    expect(normalized.categories.archive).toBe(true);
    expect(normalized.categories.program).toBe(true);
    expect(normalized.categories.document).toBe(true);
    expect(normalized.categories.model).toBe(true);
    expect(normalized.categories.other).toBe(true);
    expect("bogus" in normalized.categories).toBe(false);
  });

  it("clamps the minimum size into the safe integer range", () => {
    expect(normalizeSettings({ minimumSizeBytes: -1 }).minimumSizeBytes).toBe(0);
    expect(normalizeSettings({ minimumSizeBytes: 0 }).minimumSizeBytes).toBe(0);
    expect(normalizeSettings({ minimumSizeBytes: Number.MAX_SAFE_INTEGER }).minimumSizeBytes).toBe(
      Number.MAX_SAFE_INTEGER,
    );
    expect(
      normalizeSettings({ minimumSizeBytes: Number.MAX_SAFE_INTEGER + 100 }).minimumSizeBytes,
    ).toBe(Number.MAX_SAFE_INTEGER);
  });

  it("normalizes blacklist entries to lowercase hostnames and drops wildcards or invalid values", () => {
    const normalized = normalizeSettings({
      blacklistHosts: [
        "HTTPS://Example.COM:8080/path",
        "*.evil.test",
        "bad host",
        "http://Sub.Example.com/",
        "http://",
      ],
    });

    expect([...normalized.blacklistHosts].sort()).toEqual(["example.com", "sub.example.com"]);
  });

  it("does not enable takeover from a non-boolean value", () => {
    const normalized = normalizeSettings({ takeoverEnabled: "yes" });

    expect(normalized.takeoverEnabled).toBe(false);
  });
});

describe("settings document merging", () => {
  it("preserves unknown top-level fields and replaces the extension fields including sendCookies", () => {
    const existing = {
      schemaVersion: 1,
      theme: "dark",
      takeoverEnabled: false,
      sendCookies: true,
      categories: allCategories(true),
      minimumSizeBytes: 1,
      blacklistHosts: ["old.test"],
      custom: { note: "keep" },
    };
    const replacement = settings({ sendCookies: false, takeoverEnabled: true });

    const merged = mergeSettingsDocument(existing, replacement);

    expect(merged.schemaVersion).toBe(1);
    expect(merged.theme).toBe("dark");
    expect(merged.custom).toEqual({ note: "keep" });
    expect(merged.takeoverEnabled).toBe(true);
    expect(merged.sendCookies).toBe(false);
    expect(merged.categories).toEqual(replacement.categories);
    expect(merged.minimumSizeBytes).toBe(2048);
    expect(merged.blacklistHosts).toEqual(["example.com"]);
    expect(existing.takeoverEnabled).toBe(false);
    expect(existing.sendCookies).toBe(true);
  });

  it("creates a fresh document when the existing value is not an object", () => {
    const replacement = settings({ sendCookies: true });

    expect(mergeSettingsDocument(null, replacement)).toEqual(replacement);
    expect(mergeSettingsDocument("corrupt", replacement)).toEqual(replacement);
  });
});

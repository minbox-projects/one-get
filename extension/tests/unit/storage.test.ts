import { describe, expect, it } from "vitest";

import {
  STORAGE_KEYS,
  clearToken,
  loadSettingsDocument,
  loadToken,
  saveSettingsDocument,
  saveToken,
} from "../../shared/storage";
import type { StorageArea } from "../../shared/storage";
import type { ExtensionSettings } from "../../shared/settings";

const TOKEN = "0123456789abcdef".repeat(4);

function createArea(initial: Record<string, unknown> = {}) {
  const store: Record<string, unknown> = { ...initial };
  const area: StorageArea = {
    async get(keys) {
      if (keys === null) {
        return { ...store };
      }
      const requested = Array.isArray(keys) ? keys : [keys];
      return Object.fromEntries(
        requested.filter((key) => key in store).map((key) => [key, store[key]]),
      );
    },
    async set(items) {
      Object.assign(store, items);
    },
    async remove(keys) {
      for (const key of Array.isArray(keys) ? keys : [keys]) {
        delete store[key];
      }
    },
  };

  return { area, store };
}

function settings(): ExtensionSettings {
  return {
    takeoverEnabled: true,
    categories: {
      video: true,
      audio: true,
      archive: true,
      program: true,
      document: true,
      model: true,
      other: true,
    },
    minimumSizeBytes: 4096,
    blacklistHosts: ["example.com"],
  };
}

describe("extension storage keys", () => {
  it("reserves persistent storage for the token and settings only", () => {
    expect(STORAGE_KEYS).toEqual({
      token: "oneget.token",
      settings: "oneget.settings",
    });

    for (const key of Object.values(STORAGE_KEYS)) {
      expect(key).not.toMatch(/resource|page/i);
    }
  });
});

describe("pairing token storage", () => {
  it("loads a 64-character lowercase hex token", async () => {
    const { area } = createArea({ [STORAGE_KEYS.token]: TOKEN });

    await expect(loadToken(area)).resolves.toBe(TOKEN);
  });

  it("rejects tokens with the wrong shape or type", async () => {
    const invalid: unknown[] = [
      TOKEN.toUpperCase(),
      "a".repeat(63),
      "a".repeat(65),
      "g".repeat(64),
      123,
      null,
    ];

    for (const value of invalid) {
      const { area } = createArea({ [STORAGE_KEYS.token]: value });
      await expect(loadToken(area), String(value)).resolves.toBeNull();
    }

    await expect(loadToken(createArea().area)).resolves.toBeNull();
  });

  it("saves and clears the token under the dedicated key", async () => {
    const { area, store } = createArea();

    await saveToken(area, TOKEN);
    expect(store[STORAGE_KEYS.token]).toBe(TOKEN);

    await clearToken(area);
    expect(store[STORAGE_KEYS.token]).toBeUndefined();
  });
});

describe("settings document storage", () => {
  it("loads a stored settings document", async () => {
    const document = { schemaVersion: 2, theme: "dark" };
    const { area } = createArea({ [STORAGE_KEYS.settings]: document });

    await expect(loadSettingsDocument(area)).resolves.toEqual(document);
  });

  it("treats an absent, corrupt, or unreadable document as undefined", async () => {
    await expect(loadSettingsDocument(createArea().area)).resolves.toBeUndefined();
    await expect(
      loadSettingsDocument(createArea({ [STORAGE_KEYS.settings]: "not a document" }).area),
    ).resolves.toBeUndefined();
    await expect(
      loadSettingsDocument(createArea({ [STORAGE_KEYS.settings]: 42 }).area),
    ).resolves.toBeUndefined();

    const failing: StorageArea = {
      async get() {
        throw new Error("storage unavailable");
      },
      async set() {},
      async remove() {},
    };
    await expect(loadSettingsDocument(failing)).resolves.toBeUndefined();
  });

  it("saves a merged document without losing unknown top-level fields", async () => {
    const { area, store } = createArea();
    const replacement = settings();
    const existing = {
      schemaVersion: 2,
      theme: "dark",
      takeoverEnabled: false,
      categories: {},
      minimumSizeBytes: 1,
      blacklistHosts: ["old.test"],
      custom: { note: "keep" },
    };

    await saveSettingsDocument(area, existing, replacement);

    expect(store[STORAGE_KEYS.settings]).toEqual({
      schemaVersion: 2,
      theme: "dark",
      takeoverEnabled: true,
      categories: replacement.categories,
      minimumSizeBytes: 4096,
      blacklistHosts: ["example.com"],
      custom: { note: "keep" },
    });
  });
});

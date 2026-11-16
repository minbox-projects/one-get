import type { ResourceCategory } from "./types";
import { RESOURCE_CATEGORIES } from "./types";

export interface ExtensionSettings {
  takeoverEnabled: boolean;
  categories: Record<ResourceCategory, boolean>;
  minimumSizeBytes: number;
  blacklistHosts: string[];
  /**
   * Opt-in cookie capture. Stored documents written before this field existed
   * omit it, so it stays optional on input; `normalizeSettings` always fills a
   * concrete boolean and any value other than `true` resolves to off.
   */
  sendCookies?: boolean;
}

export type SettingsDocument = Record<string, unknown> & ExtensionSettings;

export const DEFAULT_SETTINGS: ExtensionSettings = {
  takeoverEnabled: false,
  categories: {
    video: true,
    audio: true,
    archive: true,
    program: true,
    document: true,
    model: true,
    other: true,
  },
  minimumSizeBytes: 1024 * 1024,
  blacklistHosts: [],
  sendCookies: false,
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function cloneCategories(
  source: Record<ResourceCategory, boolean>,
): Record<ResourceCategory, boolean> {
  return {
    video: source.video,
    audio: source.audio,
    archive: source.archive,
    program: source.program,
    document: source.document,
    model: source.model,
    other: source.other,
  };
}

function normalizeMinimumSize(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return DEFAULT_SETTINGS.minimumSizeBytes;
  }
  return Math.min(Math.max(Math.floor(value), 0), Number.MAX_SAFE_INTEGER);
}

function normalizeHostEntry(entry: string): string | null {
  const trimmed = entry.trim();
  if (trimmed.length === 0 || trimmed.includes("*")) {
    return null;
  }

  let parsed: URL;
  try {
    parsed = new URL(trimmed.includes("://") ? trimmed : `http://${trimmed}`);
  } catch {
    return null;
  }

  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    return null;
  }
  if (parsed.username.length > 0 || parsed.password.length > 0) {
    return null;
  }

  const hostname = parsed.hostname.toLowerCase();
  if (hostname.length === 0 || /\s/.test(hostname)) {
    return null;
  }
  return hostname;
}

function normalizeBlacklist(value: unknown): string[] {
  if (!Array.isArray(value)) {
    return [];
  }

  const hosts: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string") {
      continue;
    }
    const hostname = normalizeHostEntry(entry);
    if (hostname !== null && !hosts.includes(hostname)) {
      hosts.push(hostname);
    }
  }
  return hosts;
}

export function normalizeSettings(raw: unknown): ExtensionSettings {
  if (!isRecord(raw)) {
    return {
      ...DEFAULT_SETTINGS,
      categories: cloneCategories(DEFAULT_SETTINGS.categories),
      blacklistHosts: [],
    };
  }

  const categories = cloneCategories(DEFAULT_SETTINGS.categories);
  if (isRecord(raw.categories)) {
    for (const category of RESOURCE_CATEGORIES) {
      const value = raw.categories[category];
      categories[category] = typeof value === "boolean" ? value : true;
    }
  }

  return {
    takeoverEnabled: raw.takeoverEnabled === true,
    categories,
    minimumSizeBytes: normalizeMinimumSize(raw.minimumSizeBytes),
    blacklistHosts: normalizeBlacklist(raw.blacklistHosts),
    sendCookies: raw.sendCookies === true,
  };
}

export function mergeSettingsDocument(
  existing: unknown,
  replacement: ExtensionSettings,
): SettingsDocument {
  const base = isRecord(existing) ? { ...existing } : {};
  return {
    ...base,
    takeoverEnabled: replacement.takeoverEnabled,
    categories: cloneCategories(replacement.categories),
    minimumSizeBytes: replacement.minimumSizeBytes,
    blacklistHosts: [...replacement.blacklistHosts],
    // Only write the opt-in when the caller supplied it, so a legacy caller
    // that omits the field leaves any existing value untouched.
    ...(replacement.sendCookies === undefined
      ? {}
      : { sendCookies: replacement.sendCookies === true }),
  };
}

import type { ExtensionSettings, SettingsDocument } from "./settings";
import { mergeSettingsDocument } from "./settings";

export const STORAGE_KEYS = {
  token: "oneget.token",
  settings: "oneget.settings",
} as const;

export interface StorageArea {
  get(keys: string | string[] | null): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
  remove(keys: string | string[]): Promise<void>;
}

const TOKEN_PATTERN = /^[0-9a-f]{64}$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function isValidToken(value: unknown): value is string {
  return typeof value === "string" && TOKEN_PATTERN.test(value);
}

export async function loadToken(area: StorageArea): Promise<string | null> {
  try {
    const values = await area.get(STORAGE_KEYS.token);
    const value = values[STORAGE_KEYS.token];
    return isValidToken(value) ? value : null;
  } catch {
    return null;
  }
}

export async function saveToken(area: StorageArea, token: string): Promise<void> {
  await area.set({ [STORAGE_KEYS.token]: token });
}

export async function clearToken(area: StorageArea): Promise<void> {
  await area.remove(STORAGE_KEYS.token);
}

export async function loadSettingsDocument(
  area: StorageArea,
): Promise<Record<string, unknown> | undefined> {
  try {
    const values = await area.get(STORAGE_KEYS.settings);
    const value = values[STORAGE_KEYS.settings];
    return isRecord(value) ? value : undefined;
  } catch {
    return undefined;
  }
}

export async function saveSettingsDocument(
  area: StorageArea,
  existing: unknown,
  replacement: ExtensionSettings,
): Promise<void> {
  const document: SettingsDocument = mergeSettingsDocument(existing, replacement);
  await area.set({ [STORAGE_KEYS.settings]: document });
}

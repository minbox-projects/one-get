import type { ResourceCategory, ResourceKind } from "./types";

export interface TakeoverRules {
  takeoverEnabled: boolean;
  categories: Record<ResourceCategory, boolean>;
  minimumSizeBytes: number;
  blacklistHosts: readonly string[];
}

export interface TakeoverCandidate {
  url: string;
  kind: ResourceKind;
  category: ResourceCategory | null;
  size: number | null;
  incognito?: boolean;
  fromExtension?: boolean;
}

export type TakeoverReason =
  | "eligible"
  | "disabled"
  | "non_http"
  | "incognito"
  | "extension_download"
  | "blacklisted"
  | "manifest"
  | "unsupported_kind"
  | "category_disabled"
  | "too_small";

export type TakeoverDecision =
  | { takeOver: true; reason: "eligible" }
  | { takeOver: false; reason: Exclude<TakeoverReason, "eligible"> };

export function hostOf(url: string): string | null {
  try {
    const hostname = new URL(url).hostname.toLowerCase();
    return hostname.length > 0 ? hostname : null;
  } catch {
    return null;
  }
}

export function isBlacklistedHost(host: string, entries: readonly string[]): boolean {
  const normalizedHost = host.trim().toLowerCase();
  if (normalizedHost.length === 0) {
    return false;
  }

  return entries.some((entry) => {
    const normalizedEntry = entry.trim().toLowerCase();
    if (normalizedEntry.length === 0) {
      return false;
    }
    return normalizedHost === normalizedEntry || normalizedHost.endsWith(`.${normalizedEntry}`);
  });
}

export function decideTakeover(
  candidate: TakeoverCandidate,
  rules: TakeoverRules,
): TakeoverDecision {
  if (!rules.takeoverEnabled) {
    return { takeOver: false, reason: "disabled" };
  }

  const host = hostOf(candidate.url);
  if (host === null) {
    return { takeOver: false, reason: "non_http" };
  }

  try {
    const protocol = new URL(candidate.url).protocol;
    if (protocol !== "http:" && protocol !== "https:") {
      return { takeOver: false, reason: "non_http" };
    }
  } catch {
    return { takeOver: false, reason: "non_http" };
  }

  if (candidate.incognito === true) {
    return { takeOver: false, reason: "incognito" };
  }
  if (candidate.fromExtension === true) {
    return { takeOver: false, reason: "extension_download" };
  }
  if (isBlacklistedHost(host, rules.blacklistHosts)) {
    return { takeOver: false, reason: "blacklisted" };
  }
  if (candidate.kind === "hls" || candidate.kind === "dash") {
    return { takeOver: false, reason: "manifest" };
  }
  if (candidate.kind === "video-platform") {
    return { takeOver: false, reason: "unsupported_kind" };
  }

  const category = candidate.category;
  if (category === null || rules.categories[category] !== true) {
    return { takeOver: false, reason: "category_disabled" };
  }

  if (candidate.size !== null && candidate.size < rules.minimumSizeBytes) {
    return { takeOver: false, reason: "too_small" };
  }

  return { takeOver: true, reason: "eligible" };
}

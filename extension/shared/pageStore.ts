import { classifyResource, isSegmentUrl } from "./classify";
import { displayFilename } from "./filename";
import type { ResourceEntry, ResourceInput } from "./types";

interface TabState {
  entries: Map<string, ResourceEntry>;
  pageUrl: string;
}

export interface ResourceStoreOptions {
  maxPerTab?: number;
  maxTabs?: number;
}

const DEFAULT_MAX_PER_TAB = 500;
const DEFAULT_MAX_TABS = 50;

function positiveBound(value: number | undefined, fallback: number): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return fallback;
  }
  return Math.max(1, Math.floor(value));
}

function normalizeSize(size: number | null | undefined): number | null {
  if (typeof size !== "number" || !Number.isFinite(size)) {
    return null;
  }
  return Math.max(0, size);
}

export class ResourceStore {
  private readonly tabs = new Map<number, TabState>();

  private readonly maxPerTab: number;

  private readonly maxTabs: number;

  constructor(options: ResourceStoreOptions = {}) {
    this.maxPerTab = positiveBound(options.maxPerTab, DEFAULT_MAX_PER_TAB);
    this.maxTabs = positiveBound(options.maxTabs, DEFAULT_MAX_TABS);
  }

  add(input: ResourceInput): ResourceEntry | null {
    let parsed: URL;
    try {
      parsed = new URL(input.url);
    } catch {
      return null;
    }

    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return null;
    }

    parsed.hash = "";
    const url = parsed.toString();
    if (isSegmentUrl(url)) {
      return null;
    }

    const classification = classifyResource({
      url,
      mime: input.mime,
      filename: input.filename,
    });

    let tab = this.tabs.get(input.tabId);
    if (tab === undefined) {
      tab = { entries: new Map(), pageUrl: input.pageUrl };
      this.tabs.set(input.tabId, tab);
    }

    const pageUrl = input.pageUrl.length > 0 ? input.pageUrl : tab.pageUrl;
    tab.pageUrl = pageUrl;

    const existing = tab.entries.get(url);
    const entry: ResourceEntry = {
      url,
      tabId: input.tabId,
      frameId: input.frameId,
      pageUrl,
      capturedAtMs: input.nowMs,
      filename: displayFilename(url, input.filename ?? existing?.filename),
      mime: typeof input.mime === "string" && input.mime.length > 0 ? input.mime : (existing?.mime ?? null),
      size: input.size === undefined ? (existing?.size ?? null) : normalizeSize(input.size),
      kind: classification.kind,
      category: classification.category,
    };

    tab.entries.set(url, entry);
    this.enforceTabBound(tab);
    this.touchTab(input.tabId);
    this.enforceTabCount();
    return entry;
  }

  list(tabId: number): ResourceEntry[] {
    const tab = this.tabs.get(tabId);
    if (tab === undefined) {
      return [];
    }
    return Array.from(tab.entries.values(), (entry) => ({ ...entry }));
  }

  size(): number {
    let total = 0;
    for (const tab of this.tabs.values()) {
      total += tab.entries.size;
    }
    return total;
  }

  trackedTabs(): number {
    return this.tabs.size;
  }

  clearFrame(tabId: number, frameId: number): void {
    const tab = this.tabs.get(tabId);
    if (tab === undefined) {
      return;
    }
    for (const [url, entry] of tab.entries) {
      if (entry.frameId === frameId) {
        tab.entries.delete(url);
      }
    }
    this.touchTab(tabId);
  }

  navigateTab(tabId: number, pageUrl: string): void {
    const tab = this.tabs.get(tabId);
    if (tab === undefined) {
      return;
    }
    tab.entries.clear();
    tab.pageUrl = pageUrl;
    this.touchTab(tabId);
  }

  navigateFrame(tabId: number, frameId: number, pageUrl: string): void {
    const tab = this.tabs.get(tabId);
    if (tab === undefined) {
      return;
    }
    for (const [url, entry] of tab.entries) {
      if (entry.frameId === frameId) {
        tab.entries.delete(url);
      }
    }
    if (frameId === 0) {
      tab.pageUrl = pageUrl;
    }
    this.touchTab(tabId);
  }

  clearTab(tabId: number): void {
    this.tabs.delete(tabId);
  }

  clear(): void {
    this.tabs.clear();
  }

  private touchTab(tabId: number): void {
    const tab = this.tabs.get(tabId);
    if (tab === undefined) {
      return;
    }
    this.tabs.delete(tabId);
    this.tabs.set(tabId, tab);
  }

  private enforceTabBound(tab: TabState): void {
    while (tab.entries.size > this.maxPerTab) {
      let oldestUrl: string | null = null;
      let oldestAt = Number.POSITIVE_INFINITY;
      for (const [url, entry] of tab.entries) {
        if (entry.capturedAtMs < oldestAt) {
          oldestAt = entry.capturedAtMs;
          oldestUrl = url;
        }
      }
      if (oldestUrl === null) {
        return;
      }
      tab.entries.delete(oldestUrl);
    }
  }

  private enforceTabCount(): void {
    while (this.tabs.size > this.maxTabs) {
      const oldest = this.tabs.keys().next();
      if (oldest.done === true) {
        return;
      }
      this.tabs.delete(oldest.value);
    }
  }
}

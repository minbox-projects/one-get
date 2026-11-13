import { describe, expect, it } from "vitest";

import { ResourceStore } from "../../shared/pageStore";
import type { ResourceInput } from "../../shared/types";

function resource(
  url: string,
  tabId = 1,
  overrides: Partial<ResourceInput> = {},
): ResourceInput {
  return {
    url,
    tabId,
    frameId: 0,
    pageUrl: "https://page.test/",
    nowMs: 1000,
    ...overrides,
  };
}

function urls(store: ResourceStore, tabId: number): string[] {
  return store
    .list(tabId)
    .map((entry) => entry.url)
    .sort();
}

describe("resource store ownership", () => {
  it("deduplicates the same tab and URL while ignoring a fragment difference", () => {
    const store = new ResourceStore();

    expect(store.add(resource("https://cdn.test/files/a.mp4#first"))).not.toBeNull();
    expect(store.add(resource("https://cdn.test/files/a.mp4#second"))).not.toBeNull();

    expect(store.list(1)).toHaveLength(1);
    expect(store.list(1)[0].url).toBe("https://cdn.test/files/a.mp4");
    expect(store.size()).toBe(1);
  });

  it("keeps identical URLs in different tabs separate", () => {
    const store = new ResourceStore();

    store.add(resource("https://cdn.test/files/a.mp4", 1));
    store.add(resource("https://cdn.test/files/a.mp4", 2));

    expect(store.size()).toBe(2);
    expect(store.list(1)).toHaveLength(1);
    expect(store.list(2)).toHaveLength(1);
    expect(store.trackedTabs()).toBe(2);
  });

  it("filters segment URLs while keeping manifests", () => {
    const store = new ResourceStore();

    expect(store.add(resource("https://cdn.test/seg/1.ts"))).toBeNull();
    expect(store.add(resource("https://cdn.test/seg/init.mp4"))).toBeNull();
    expect(store.add(resource("https://cdn.test/live/master.m3u8"))).toMatchObject({
      kind: "hls",
      category: null,
    });

    expect(store.size()).toBe(1);
    expect(urls(store, 1)).toEqual(["https://cdn.test/live/master.m3u8"]);
  });

  it("clears one frame without disturbing the other frames in a tab", () => {
    const store = new ResourceStore();

    store.add(resource("https://cdn.test/frame-0.mp4", 1, { frameId: 0 }));
    store.add(resource("https://cdn.test/frame-2.mp4", 1, { frameId: 2 }));

    store.clearFrame(1, 2);

    expect(urls(store, 1)).toEqual(["https://cdn.test/frame-0.mp4"]);
    expect(store.size()).toBe(1);
  });

  it("clears a tab on navigation and attributes later resources to the new page", () => {
    const store = new ResourceStore();

    store.add(resource("https://cdn.test/old.mp4", 1, { pageUrl: "https://page.test/old" }));
    store.navigateTab(1, "https://page.test/new");

    expect(store.list(1)).toEqual([]);

    store.add(
      resource("https://cdn.test/new.mp4", 1, {
        pageUrl: "https://page.test/new",
        nowMs: 2000,
      }),
    );

    expect(store.list(1)).toHaveLength(1);
    expect(store.list(1)[0]).toMatchObject({
      url: "https://cdn.test/new.mp4",
      pageUrl: "https://page.test/new",
      capturedAtMs: 2000,
    });
  });

  it("clears only the navigating frame and keeps the other frames", () => {
    const store = new ResourceStore();

    store.add(
      resource("https://cdn.test/frame-0.mp4", 1, {
        frameId: 0,
        pageUrl: "https://page.test/",
      }),
    );
    store.add(
      resource("https://cdn.test/frame-1.mp4", 1, {
        frameId: 1,
        pageUrl: "https://frame.test/old",
      }),
    );

    store.navigateFrame(1, 1, "https://frame.test/new");
    store.add(
      resource("https://cdn.test/frame-1-new.mp4", 1, {
        frameId: 1,
        pageUrl: "https://frame.test/new",
      }),
    );

    const entries = store.list(1);
    expect(entries).toHaveLength(2);
    expect(entries.map((entry) => entry.url).sort()).toEqual([
      "https://cdn.test/frame-0.mp4",
      "https://cdn.test/frame-1-new.mp4",
    ]);
    expect(entries.filter((entry) => entry.frameId === 1)[0].pageUrl).toBe(
      "https://frame.test/new",
    );
  });

  it("evicts the oldest resource when a tab exceeds its bound", () => {
    const store = new ResourceStore({ maxPerTab: 2 });

    store.add(resource("https://cdn.test/a.mp4", 1, { nowMs: 1 }));
    store.add(resource("https://cdn.test/b.mp4", 1, { nowMs: 2 }));
    store.add(resource("https://cdn.test/c.mp4", 1, { nowMs: 3 }));

    expect(urls(store, 1)).toEqual(["https://cdn.test/b.mp4", "https://cdn.test/c.mp4"]);
    expect(store.size()).toBe(2);
  });

  it("evicts the least recently used tab when the tab bound is exceeded", () => {
    const store = new ResourceStore({ maxTabs: 2 });

    store.add(resource("https://cdn.test/tab-1-a.mp4", 1));
    store.add(resource("https://cdn.test/tab-2-a.mp4", 2));
    store.add(resource("https://cdn.test/tab-1-b.mp4", 1));
    store.add(resource("https://cdn.test/tab-3-a.mp4", 3));

    expect(store.trackedTabs()).toBe(2);
    expect(urls(store, 1)).toEqual([
      "https://cdn.test/tab-1-a.mp4",
      "https://cdn.test/tab-1-b.mp4",
    ]);
    expect(store.list(2)).toEqual([]);
    expect(urls(store, 3)).toEqual(["https://cdn.test/tab-3-a.mp4"]);
  });

  // A4: a later observation without a size must not erase a known size.
  it("keeps a known resource size when a later observation reports null", () => {
    const store = new ResourceStore();
    const url = "https://cdn.test/files/sized.mp4";

    const first = store.add(resource(url, 1, { size: 4_194_304, nowMs: 1_000 }));
    const second = store.add(resource(url, 1, { size: null, nowMs: 2_000 }));

    expect(first?.size).toBe(4_194_304);
    expect(second?.size).toBe(4_194_304);
    expect(store.list(1)).toHaveLength(1);
    expect(store.list(1)[0].size).toBe(4_194_304);
  });
});

import { describe, expect, it } from "vitest";

import { createBrowserAdapter } from "../../shared/browser";
import type { BrowserApi } from "../../shared/browser";

const TOKEN_A = "0123456789abcdef".repeat(4);

type Listener = (...args: unknown[]) => unknown;

interface ChromiumMockOptions {
  sendMessage?: (message: unknown, callback: (response: unknown) => void) => void;
  lastError?: { message: string };
  tabs?: Array<{ id?: number; url?: string }>;
  storage?: Record<string, unknown>;
  session?: Record<string, unknown>;
}

function chromiumArea(store: Record<string, unknown>) {
  return {
    get(keys: string | string[] | null, callback: (values: Record<string, unknown>) => void) {
      const requested = keys === null ? Object.keys(store) : Array.isArray(keys) ? keys : [keys];
      callback(
        Object.fromEntries(
          requested.filter((key) => key in store).map((key) => [key, store[key]]),
        ),
      );
    },
    set(items: Record<string, unknown>, callback: () => void) {
      Object.assign(store, items);
      callback();
    },
    remove(keys: string | string[], callback: () => void) {
      for (const key of Array.isArray(keys) ? keys : [keys]) {
        delete store[key];
      }
      callback();
    },
  };
}

function chromiumApi(options: ChromiumMockOptions = {}) {
  const local = { ...(options.storage ?? {}) };
  const session = { ...(options.session ?? {}) };
  const listeners: Listener[] = [];
  const runtime: {
    lastError?: { message?: string };
    sendMessage: (message: unknown, callback: (response: unknown) => void) => void;
    onMessage: { addListener: (listener: Listener) => void };
  } = {
    lastError: options.lastError,
    sendMessage(message, callback) {
      if (options.sendMessage !== undefined) {
        options.sendMessage(message, callback);
        return;
      }
      callback({ ok: true });
    },
    onMessage: {
      addListener(listener) {
        listeners.push(listener);
      },
    },
  };
  const api = {
    runtime,
    tabs: {
      query(_query: unknown, callback: (tabs: Array<{ id?: number; url?: string }>) => void) {
        callback(options.tabs ?? [{ id: 4, url: "https://page.test/watch" }]);
      },
    },
    storage: {
      local: chromiumArea(local),
      session: chromiumArea(session),
    },
  };
  return { api, listeners, local, session };
}

interface FirefoxMockOptions {
  sendMessage?: (message: unknown) => Promise<unknown>;
  tabs?: Array<{ id?: number; url?: string }>;
  storage?: Record<string, unknown>;
  session?: Record<string, unknown>;
}

function firefoxArea(store: Record<string, unknown>) {
  return {
    async get(keys: string | string[] | null) {
      const requested = keys === null ? Object.keys(store) : Array.isArray(keys) ? keys : [keys];
      return Object.fromEntries(
        requested.filter((key) => key in store).map((key) => [key, store[key]]),
      );
    },
    async set(items: Record<string, unknown>) {
      Object.assign(store, items);
    },
    async remove(keys: string | string[]) {
      for (const key of Array.isArray(keys) ? keys : [keys]) {
        delete store[key];
      }
    },
  };
}

function firefoxApi(options: FirefoxMockOptions = {}) {
  const local = { ...(options.storage ?? {}) };
  const session = { ...(options.session ?? {}) };
  const listeners: Listener[] = [];
  const api = {
    runtime: {
      sendMessage: options.sendMessage ?? (async () => ({ ok: true })),
      onMessage: {
        addListener(listener: Listener) {
          listeners.push(listener);
        },
      },
    },
    tabs: {
      async query() {
        return options.tabs ?? [{ id: 5, url: "https://page.test/watch" }];
      },
    },
    storage: {
      local: firefoxArea(local),
      session: firefoxArea(session),
    },
  };
  return { api, listeners, local, session };
}

function adapterFor(api: unknown) {
  return createBrowserAdapter(api as BrowserApi);
}

describe("browser adapter", () => {
  it("normalizes callback-style messaging, active-tab lookup, and local/session storage", async () => {
    const { api, local } = chromiumApi({
      storage: { "oneget.token": TOKEN_A },
      session: { "oneget.resources": [] },
      sendMessage: (_message, callback) => {
        callback({ accepted: true });
      },
    });
    const adapter = adapterFor(api);

    await expect(adapter.sendMessage({ type: "ping" })).resolves.toEqual({ accepted: true });
    await expect(adapter.getActiveTab()).resolves.toMatchObject({
      id: 4,
      url: "https://page.test/watch",
    });

    const localArea = adapter.storage("local");
    await expect(localArea.get("oneget.token")).resolves.toEqual({ "oneget.token": TOKEN_A });
    await localArea.set({ "oneget.settings": { takeoverEnabled: true } });
    await expect(localArea.get(["oneget.token", "oneget.settings"])).resolves.toMatchObject({
      "oneget.token": TOKEN_A,
      "oneget.settings": { takeoverEnabled: true },
    });
    await localArea.remove("oneget.token");
    await expect(localArea.get("oneget.token")).resolves.toEqual({});
    expect(local["oneget.token"]).toBeUndefined();

    const sessionArea = adapter.storage("session");
    await sessionArea.set({ "oneget.resources": [{ url: "https://cdn.test/files/movie.mp4" }] });
    await expect(sessionArea.get(null)).resolves.toMatchObject({
      "oneget.resources": [{ url: "https://cdn.test/files/movie.mp4" }],
    });
    expect(local["oneget.resources"]).toBeUndefined();

    const empty = adapterFor(chromiumApi({ tabs: [] }).api);
    await expect(empty.getActiveTab()).resolves.toBeNull();
  });

  it("rejects callback errors and synchronous throws instead of throwing synchronously", async () => {
    const failed = adapterFor(
      chromiumApi({ lastError: { message: "runtime unavailable" } }).api,
    );
    await expect(failed.sendMessage({ type: "ping" })).rejects.toThrow("runtime unavailable");

    const throwing = adapterFor(
      chromiumApi({
        sendMessage: () => {
          throw new Error("sync boom");
        },
      }).api,
    );
    let pending: Promise<unknown> | undefined;
    expect(() => {
      pending = throwing.sendMessage({ type: "ping" });
    }).not.toThrow();
    await expect(pending).rejects.toThrow("sync boom");
  });

  it("normalizes promise-style messaging, active-tab lookup, and storage", async () => {
    const { api } = firefoxApi({
      storage: { "oneget.token": TOKEN_A },
      session: { "oneget.resources": [] },
      tabs: [{ id: 11, url: "https://page.test/watch" }],
      sendMessage: async (message) => ({ echoed: (message as { type?: string }).type }),
    });
    const adapter = adapterFor(api);

    await expect(adapter.sendMessage({ type: "ping" })).resolves.toEqual({ echoed: "ping" });
    await expect(adapter.getActiveTab()).resolves.toMatchObject({ id: 11 });

    const localArea = adapter.storage("local");
    await expect(localArea.get("oneget.token")).resolves.toEqual({ "oneget.token": TOKEN_A });
    await localArea.set({ "oneget.settings": { takeoverEnabled: false } });
    await expect(localArea.get("oneget.settings")).resolves.toEqual({
      "oneget.settings": { takeoverEnabled: false },
    });
    await localArea.remove("oneget.settings");
    await expect(localArea.get("oneget.settings")).resolves.toEqual({});

    const sessionArea = adapter.storage("session");
    await sessionArea.set({ "oneget.resources": [] });
    await expect(sessionArea.get("oneget.resources")).resolves.toEqual({
      "oneget.resources": [],
    });

    const failed = adapterFor(
      firefoxApi({
        sendMessage: async () => {
          throw new Error("offline");
        },
      }).api,
    );
    await expect(failed.sendMessage({ type: "ping" })).rejects.toThrow("offline");

    const empty = adapterFor(firefoxApi({ tabs: [] }).api);
    await expect(empty.getActiveTab()).resolves.toBeNull();
  });

  it("registers a callback-style message listener that responds through sendResponse", async () => {
    const { api, listeners } = chromiumApi();
    const adapter = adapterFor(api);
    const received: unknown[] = [];
    adapter.onMessage((message) => {
      received.push(message);
      return { echoed: true };
    });

    expect(listeners).toHaveLength(1);
    const response = await new Promise((resolve) => {
      const returned = listeners[0]({ type: "ping" }, { tab: { id: 9 }, frameId: 1 }, resolve);
      expect(returned).toBe(true);
    });

    expect(response).toEqual({ echoed: true });
    expect(received).toEqual([{ type: "ping" }]);
  });

  it("registers a promise-style message listener that returns the handler result", async () => {
    const { api, listeners } = firefoxApi();
    const adapter = adapterFor(api);
    adapter.onMessage(async (message) => ({ echoed: (message as { type?: string }).type }));

    expect(listeners).toHaveLength(1);
    const result = listeners[0]({ type: "ping" }, { tab: { id: 9 }, frameId: 0 });
    await expect(Promise.resolve(result)).resolves.toEqual({ echoed: "ping" });
  });

  it("reads response headers case-insensitively and ignores missing or non-string values", () => {
    const adapter = adapterFor(chromiumApi().api);
    const headers = [
      { name: "Content-Type", value: "video/mp4" },
      { name: "Content-Length", value: "2048" },
      { name: "X-Empty" },
    ];

    expect(adapter.responseHeader(headers, "content-type")).toBe("video/mp4");
    expect(adapter.responseHeader(headers, "CONTENT-LENGTH")).toBe("2048");
    expect(adapter.responseHeader(headers, "x-empty")).toBeNull();
    expect(adapter.responseHeader(headers, "missing")).toBeNull();
    expect(adapter.responseHeader(undefined, "content-type")).toBeNull();
  });
});

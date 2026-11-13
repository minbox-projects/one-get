import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import { createBackgroundController } from "../../chromium/background";
import type {
  BackgroundController,
  BrowserDownload,
  TakeoverFailure,
} from "../../chromium/background";
import { DEFAULT_SETTINGS } from "../../shared/settings";
import type { ExtensionSettings } from "../../shared/settings";
import type { StorageArea } from "../../shared/storage";
import type { ResourceInput } from "../../shared/types";

const TEST_ORIGIN = "http://127.0.0.1:37652";
const TOKEN_A = "0123456789abcdef".repeat(4);
const TOKEN_B = "fedcba9876543210".repeat(4);
const PAIRING_CODE = "12345678";
const SAFE_HEADERS = new Set(["referer", "user-agent", "accept", "accept-language"]);

interface Area {
  area: StorageArea;
  store: Record<string, unknown>;
}

function createArea(initial: Record<string, unknown> = {}): Area {
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

function savedSettings(overrides: Partial<ExtensionSettings> = {}): ExtensionSettings {
  return {
    ...DEFAULT_SETTINGS,
    categories: { ...DEFAULT_SETTINGS.categories },
    blacklistHosts: [],
    takeoverEnabled: true,
    ...overrides,
  };
}

function browserDownload(overrides: Partial<BrowserDownload> = {}): BrowserDownload {
  return {
    id: 7,
    url: "https://cdn.test/files/movie.mp4",
    filename: "movie.mp4",
    referer: "https://page.test/watch",
    mime: "video/mp4",
    totalBytes: 2 * 1024 * 1024,
    ...overrides,
  };
}

function resourceInput(url: string, overrides: Partial<ResourceInput> = {}): ResourceInput {
  return {
    url,
    tabId: 1,
    frameId: 0,
    pageUrl: "https://page.test/watch",
    nowMs: 1_000,
    ...overrides,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function takeoverAccepted(taskId = "task-1", duplicate = false): Response {
  return jsonResponse({
    intent: "takeover",
    accepted: 1,
    rejected: [],
    taskId,
    duplicate,
  });
}

interface FetchCall {
  url: string;
  init: RequestInit | undefined;
  body: unknown;
}

type FetchHandler = (request: {
  url: string;
  init: RequestInit | undefined;
  text: string | undefined;
}) => Response | Promise<Response>;

function parseBody(text: string | undefined): unknown {
  if (text === undefined || text.length === 0) {
    return undefined;
  }
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

function createFetch(handler: FetchHandler) {
  const calls: FetchCall[] = [];
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url =
      typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const text = typeof init?.body === "string" ? init.body : undefined;
    calls.push({ url, init, body: parseBody(text) });
    return handler({ url, init, text });
  }) as typeof fetch;
  return { fetchImpl, calls };
}

function createDownloads(events: string[]) {
  const downloads = {
    async cancel(id: number): Promise<void> {
      events.push(`cancel:${id}`);
    },
    async erase(query: { id: number }): Promise<void> {
      events.push(`erase:${query.id}`);
    },
  };
  return { downloads };
}

interface SetupOptions {
  token?: string;
  settings?: ExtensionSettings;
  initialStorage?: Record<string, unknown>;
  initialSession?: Record<string, unknown>;
  handler?: FetchHandler;
  events?: string[];
  log?: (event: string, fields?: Record<string, unknown>) => void;
}

function setUp(options: SetupOptions = {}) {
  const persistent = createArea({
    ...(options.token === undefined ? {} : { "oneget.token": options.token }),
    ...(options.settings === undefined ? {} : { "oneget.settings": options.settings }),
    ...options.initialStorage,
  });
  const session = createArea(options.initialSession ?? {});
  const events = options.events ?? [];
  const fetch = createFetch(options.handler ?? (() => jsonResponse({ ok: true })));
  const { downloads } = createDownloads(events);
  const controller = createBackgroundController({
    storage: persistent.area,
    session: session.area,
    fetchImpl: fetch.fetchImpl,
    apiOrigin: TEST_ORIGIN,
    now: () => 1_000,
    downloads,
    log: options.log ?? (() => {}),
  });
  return { controller, persistent, session, fetch, events };
}

type RuntimeMessageArg = Parameters<BackgroundController["handleRuntimeMessage"]>[0];

function runtimeMessage(value: Record<string, unknown>): RuntimeMessageArg {
  return value as unknown as RuntimeMessageArg;
}

function pairedValue(state: Record<string, unknown>): unknown {
  if (typeof state.paired === "boolean") {
    return state.paired;
  }
  const pairing = state.pairing;
  if (
    typeof pairing === "object" &&
    pairing !== null &&
    typeof (pairing as Record<string, unknown>).paired === "boolean"
  ) {
    return (pairing as Record<string, unknown>).paired;
  }
  if (typeof state.pairingStatus === "boolean") {
    return state.pairingStatus;
  }
  return undefined;
}

describe("download takeover", () => {
  it("reads the current token and settings, sends one takeover capture, then cancels and erases", async () => {
    const events: string[] = [];
    const { controller, persistent, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: ({ url }) => {
        events.push(`fetch:${url}`);
        return takeoverAccepted("task-42");
      },
    });

    persistent.store["oneget.token"] = TOKEN_B;
    const outcome = await controller.handleDownload(browserDownload());

    expect(outcome).toMatchObject({ action: "takeover", taskId: "task-42" });
    expect(fetch.calls).toHaveLength(1);
    const call = fetch.calls[0];
    expect(call.url).toBe(`${TEST_ORIGIN}/capture`);
    expect(call.init?.method).toBe("POST");

    const headers = new Headers(call.init?.headers);
    expect(headers.get("authorization")).toBe(`Bearer ${TOKEN_B}`);
    expect(headers.get("cookie")).toBeNull();

    const body = call.body as Record<string, unknown>;
    expect(body).toMatchObject({ protocolVersion: 1, intent: "takeover" });
    expect(typeof body.sourceId).toBe("string");
    expect((body.sourceId as string).length).toBeGreaterThan(0);

    const items = body.items as Array<Record<string, unknown>>;
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      url: "https://cdn.test/files/movie.mp4",
      filename: "movie.mp4",
      referer: "https://page.test/watch",
    });

    const itemHeaders = (items[0].headers ?? []) as Array<{ name: string }>;
    for (const header of itemHeaders) {
      expect(SAFE_HEADERS.has(header.name.toLowerCase())).toBe(true);
    }

    const serializedBody = JSON.stringify(body).toLowerCase();
    expect(serializedBody).not.toContain("cookie");
    expect(serializedBody).not.toContain("authorization");
    expect(JSON.stringify(body)).not.toContain(TOKEN_B);
    expect(events).toEqual([`fetch:${TEST_ORIGIN}/capture`, "cancel:7", "erase:7"]);

    persistent.store["oneget.settings"] = savedSettings({ takeoverEnabled: false });
    expect(await controller.handleDownload(browserDownload({ id: 8 }))).toEqual({
      action: "allow",
      reason: "disabled",
    });
    expect(fetch.calls).toHaveLength(1);
    expect(events).toEqual([`fetch:${TEST_ORIGIN}/capture`, "cancel:7", "erase:7"]);
  });

  it("leaves the browser download intact when the capture fails", async () => {
    const failures: Array<{ name: string; handler: FetchHandler }> = [
      {
        name: "a rejected request",
        handler: async () => {
          throw new Error("offline");
        },
      },
      {
        name: "a non-JSON response",
        handler: () =>
          new Response("<html>not json</html>", {
            status: 200,
            headers: { "content-type": "text/html" },
          }),
      },
      {
        name: "a server error",
        handler: () => jsonResponse({ code: "INTERNAL", message: "boom" }, 500),
      },
    ];

    for (const failure of failures) {
      const events: string[] = [];
      const { controller, fetch } = setUp({
        token: TOKEN_A,
        settings: savedSettings(),
        events,
        handler: failure.handler,
      });

      expect(await controller.handleDownload(browserDownload()), failure.name).toEqual({
        action: "allow",
        reason: "capture_failed",
      });
      expect(fetch.calls, failure.name).toHaveLength(1);
      expect(events, failure.name).toEqual([]);
    }
  });

  it("suppresses a duplicate returned by the desktop without a second cancel or erase", async () => {
    const events: string[] = [];
    let responses = 0;
    const { controller, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: () => {
        responses += 1;
        return takeoverAccepted("task-9", responses > 1);
      },
    });

    const first = await controller.handleDownload(browserDownload({ id: 1 }));
    const second = await controller.handleDownload(browserDownload({ id: 2 }));

    expect(first).toMatchObject({ action: "takeover", taskId: "task-9" });
    expect(second).toMatchObject({ reason: "duplicate", taskId: "task-9" });
    expect(fetch.calls).toHaveLength(2);
    expect(events).toEqual(["cancel:1", "erase:1"]);
  });

  it("does not repeat a local takeover for the same browser download id", async () => {
    const events: string[] = [];
    const { controller, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: () => takeoverAccepted("task-1"),
    });
    const download = browserDownload({ id: 5 });

    const first = await controller.handleDownload(download);
    const second = await controller.handleDownload({ ...download });

    expect(first.action).toBe("takeover");
    expect(second.reason).toBe("duplicate");
    expect(fetch.calls).toHaveLength(1);
    expect(events).toEqual(["cancel:5", "erase:5"]);
  });

  const ineligible: Array<{
    name: string;
    token?: string;
    settings?: ExtensionSettings;
    download?: Partial<BrowserDownload>;
    reason: string;
  }> = [
    { name: "an unpaired extension", settings: savedSettings(), reason: "unpaired" },
    {
      name: "disabled takeover",
      token: TOKEN_A,
      settings: savedSettings({ takeoverEnabled: false }),
      reason: "disabled",
    },
    {
      name: "a blacklisted subdomain",
      token: TOKEN_A,
      settings: savedSettings({ blacklistHosts: ["blocked.test"] }),
      download: { url: "https://cdn.blocked.test/movie.mp4" },
      reason: "blacklisted",
    },
    {
      name: "a disabled category",
      token: TOKEN_A,
      settings: savedSettings({
        categories: { ...savedSettings().categories, video: false },
      }),
      reason: "category_disabled",
    },
    {
      name: "a file below the minimum size",
      token: TOKEN_A,
      settings: savedSettings({ minimumSizeBytes: 1024 * 1024 }),
      download: { totalBytes: 1024 },
      reason: "too_small",
    },
    {
      name: "an HLS manifest",
      token: TOKEN_A,
      settings: savedSettings(),
      download: {
        url: "https://cdn.test/live/master.m3u8",
        filename: "master.m3u8",
        mime: "application/vnd.apple.mpegurl",
      },
      reason: "manifest",
    },
    {
      name: "a DASH manifest",
      token: TOKEN_A,
      settings: savedSettings(),
      download: {
        url: "https://cdn.test/stream/manifest.mpd",
        filename: "manifest.mpd",
        mime: "application/dash+xml",
      },
      reason: "manifest",
    },
    {
      name: "an unknown URL scheme",
      token: TOKEN_A,
      settings: savedSettings(),
      download: { url: "ftp://cdn.test/movie.mp4" },
      reason: "non_http",
    },
    {
      name: "an incognito download",
      token: TOKEN_A,
      settings: savedSettings(),
      download: { incognito: true },
      reason: "incognito",
    },
    {
      name: "an extension-originated download",
      token: TOKEN_A,
      settings: savedSettings(),
      download: { byExtension: true },
      reason: "extension_download",
    },
  ];

  it.each(ineligible)(
    "does not fetch or cancel for $name",
    async ({ token, settings, download, reason }) => {
      const events: string[] = [];
      const { controller, fetch } = setUp({
        token,
        settings,
        events,
        handler: () => jsonResponse({ ok: true }),
      });

      expect(await controller.handleDownload(browserDownload(download))).toEqual({
        action: "allow",
        reason,
      });
      expect(fetch.calls).toHaveLength(0);
      expect(events).toEqual([]);
    },
  );

  it("applies saved settings to the next download without recreating the controller", async () => {
    const events: string[] = [];
    const { controller, persistent, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings({ takeoverEnabled: false }),
      events,
      handler: () => takeoverAccepted("task-1"),
    });

    expect(await controller.handleDownload(browserDownload({ id: 1 }))).toEqual({
      action: "allow",
      reason: "disabled",
    });

    const saved = await controller.saveSettings(
      savedSettings({ takeoverEnabled: true, minimumSizeBytes: 4 * 1024 * 1024 }),
    );
    expect(saved).toMatchObject({ takeoverEnabled: true, minimumSizeBytes: 4 * 1024 * 1024 });
    expect(persistent.store["oneget.settings"]).toMatchObject({
      takeoverEnabled: true,
      minimumSizeBytes: 4 * 1024 * 1024,
    });

    expect(
      await controller.handleDownload(browserDownload({ id: 2, totalBytes: 2 * 1024 * 1024 })),
    ).toEqual({ action: "allow", reason: "too_small" });
    expect(fetch.calls).toHaveLength(0);

    expect(
      (await controller.handleDownload(browserDownload({ id: 3, totalBytes: 8 * 1024 * 1024 })))
        .action,
    ).toBe("takeover");
    expect(fetch.calls).toHaveLength(1);
  });

  it("clears the stored token on 401 and treats the following download as unpaired", async () => {
    const events: string[] = [];
    const { controller, persistent, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: () => jsonResponse({ code: "UNAUTHORIZED", message: "revoked" }, 401),
    });

    expect(await controller.handleDownload(browserDownload({ id: 1 }))).toEqual({
      action: "allow",
      reason: "token_revoked",
    });
    expect(persistent.store["oneget.token"]).toBeUndefined();

    expect(await controller.handleDownload(browserDownload({ id: 2 }))).toEqual({
      action: "allow",
      reason: "unpaired",
    });
    expect(fetch.calls).toHaveLength(1);
    expect(events).toEqual([]);
  });

  it("keeps the stored token and reports update guidance on 409", async () => {
    const events: string[] = [];
    const { controller, persistent } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: () =>
        jsonResponse({ code: "PROTOCOL_MISMATCH", message: "update OneGet" }, 409),
    });

    expect(await controller.handleDownload(browserDownload())).toEqual({
      action: "allow",
      reason: "update_required",
    });
    expect(persistent.store["oneget.token"]).toBe(TOKEN_A);
    expect(events).toEqual([]);
  });

  it("keeps the token and pairing code out of log fields", async () => {
    const logs: Array<{ event: string; fields?: Record<string, unknown> }> = [];
    const { controller } = setUp({
      settings: savedSettings(),
      log: (event, fields) => {
        logs.push({ event, fields });
      },
      handler: ({ url }) => {
        if (url.endsWith("/pair")) {
          return jsonResponse({
            token: TOKEN_A,
            client: {
              id: "client-1",
              origin: "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
              label: "Chrome",
            },
          });
        }
        return jsonResponse({ code: "UNAUTHORIZED", message: "revoked" }, 401);
      },
    });

    expect((await controller.pair(PAIRING_CODE)).ok).toBe(true);
    await controller.handleDownload(browserDownload());

    expect(logs.length).toBeGreaterThan(0);
    const serialized = JSON.stringify(logs);
    expect(serialized).not.toContain(TOKEN_A);
    expect(serialized).not.toContain(PAIRING_CODE);
  });

  it("keeps page resources in session storage only and filters segments", async () => {
    const { controller, persistent, session } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
    });

    await controller.observeResource(resourceInput("https://cdn.test/files/movie.mp4#one"));
    await controller.observeResource(resourceInput("https://cdn.test/files/movie.mp4#two"));
    await controller.observeResource(resourceInput("https://cdn.test/seg/1.ts"));
    await controller.observeResource(resourceInput("https://cdn.test/live/master.m3u8"));

    const list = await controller.listResources(1);
    expect(list.map((entry) => entry.url).sort()).toEqual([
      "https://cdn.test/files/movie.mp4",
      "https://cdn.test/live/master.m3u8",
    ]);
    expect(Object.keys(persistent.store).sort()).toEqual(["oneget.settings", "oneget.token"]);
    expect(Object.keys(session.store).length).toBeGreaterThan(0);
    expect(JSON.stringify(session.store)).not.toContain(TOKEN_A);
  });
});

describe("runtime messages", () => {
  it("returns popup state with resources, pairing status, and settings", async () => {
    const { controller } = setUp({ token: TOKEN_A, settings: savedSettings() });
    await controller.observeResource(resourceInput("https://cdn.test/files/movie.mp4"));

    const state = await controller.handleRuntimeMessage(
      runtimeMessage({ type: "popup/get-state", tabId: 1 }),
      { tabId: 1 },
    );
    const record = state as Record<string, unknown>;
    expect(Array.isArray(record.resources)).toBe(true);
    expect(record.resources).toHaveLength(1);
    expect(record.settings).toMatchObject({ takeoverEnabled: true });
    expect(pairedValue(record)).toBe(true);
  });

  it("clears only the navigating frame's resources for a subframe SPA and still clears the tab for the top frame", async () => {
    const { controller, session } = setUp({ token: TOKEN_A, settings: savedSettings() });
    const topUrl = "https://cdn.test/files/top.mp4";
    const subframeUrl = "https://cdn.test/files/embedded.mp4";
    const laterSubframeUrl = "https://cdn.test/files/embedded-next.mp4";

    await controller.observeResource(resourceInput(topUrl, { frameId: 0 }));
    await controller.observeResource(
      resourceInput(subframeUrl, { frameId: 1, pageUrl: "https://embed.test/player" }),
    );

    await controller.handleRuntimeMessage(
      runtimeMessage({ type: "content/spa", pageUrl: "https://embed.test/next" }),
      { tabId: 1, frameId: 1, pageUrl: "https://embed.test/next" },
    );

    expect(await controller.listResources(1)).toMatchObject([
      { url: topUrl, frameId: 0 },
    ]);
    const afterSubframeSpa = JSON.stringify(session.store);
    expect(afterSubframeSpa).toContain(topUrl);
    expect(afterSubframeSpa).not.toContain(subframeUrl);

    await controller.observeResource(
      resourceInput(laterSubframeUrl, { frameId: 1, pageUrl: "https://embed.test/next" }),
    );

    await controller.handleRuntimeMessage(
      runtimeMessage({ type: "content/spa", pageUrl: "https://page.test/next" }),
      { tabId: 1, frameId: 0, pageUrl: "https://page.test/next" },
    );

    expect(await controller.listResources(1)).toEqual([]);
    const afterTopFrameSpa = JSON.stringify(session.store);
    expect(afterTopFrameSpa).not.toContain(topUrl);
    expect(afterTopFrameSpa).not.toContain(laterSubframeUrl);
  });

  it("sends one prefill capture for a listed resource", async () => {
    const events: string[] = [];
    const { controller, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      events,
      handler: () =>
        jsonResponse({ intent: "prefill", accepted: 1, rejected: [], duplicate: false }),
    });
    await controller.observeResource(resourceInput("https://cdn.test/files/movie.mp4"));
    const [listed] = await controller.listResources(1);
    expect(listed).toBeDefined();

    await controller.handleRuntimeMessage(
      runtimeMessage({
        type: "popup/capture-prefill",
        tabId: 1,
        url: listed.url,
        resourceUrl: listed.url,
        resource: listed,
        item: listed,
      }),
      { tabId: 1 },
    );

    expect(fetch.calls).toHaveLength(1);
    const call = fetch.calls[0];
    expect(call.url).toBe(`${TEST_ORIGIN}/capture`);
    expect(new Headers(call.init?.headers).get("authorization")).toBe(`Bearer ${TOKEN_A}`);
    const body = call.body as Record<string, unknown>;
    expect(body).toMatchObject({ protocolVersion: 1, intent: "prefill" });
    const items = body.items as Array<Record<string, unknown>>;
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      url: "https://cdn.test/files/movie.mp4",
      filename: "movie.mp4",
    });
    expect(events).toEqual([]);
  });

  it("stores the token returned by popup pairing", async () => {
    const { controller, persistent, fetch } = setUp({
      settings: savedSettings(),
      handler: () =>
        jsonResponse({
          token: TOKEN_A,
          client: {
            id: "client-1",
            origin: "chrome-extension://abcdefghijklmnopabcdefghijklmnop",
            label: "Chrome",
          },
        }),
    });

    await controller.handleRuntimeMessage(runtimeMessage({ type: "popup/pair", code: PAIRING_CODE }));

    expect(persistent.store["oneget.token"]).toBe(TOKEN_A);
    expect(fetch.calls).toHaveLength(1);
    expect(fetch.calls[0].url).toBe(`${TEST_ORIGIN}/pair`);
    expect(fetch.calls[0].body).toMatchObject({ protocolVersion: 1, code: PAIRING_CODE });
  });

  it("clears the stored token on popup unpair", async () => {
    const { controller, persistent } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      handler: () => jsonResponse({ revoked: true }),
    });

    await controller.handleRuntimeMessage(runtimeMessage({ type: "popup/unpair" }));

    expect(persistent.store["oneget.token"]).toBeUndefined();
    expect(persistent.store["oneget.settings"]).toMatchObject({ takeoverEnabled: true });
  });

  it("updates settings from a settings/changed message and applies them to the next decision", async () => {
    const events: string[] = [];
    const { controller, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings({ takeoverEnabled: false }),
      events,
      handler: () => takeoverAccepted("task-1"),
    });

    await controller.handleRuntimeMessage(
      runtimeMessage({
        type: "settings/changed",
        settings: savedSettings({ takeoverEnabled: true, blacklistHosts: ["blocked.test"] }),
      }),
    );

    expect(await controller.getSettings()).toMatchObject({
      takeoverEnabled: true,
      blacklistHosts: ["blocked.test"],
    });
    expect(
      await controller.handleDownload(browserDownload({ url: "https://cdn.blocked.test/movie.mp4" })),
    ).toEqual({ action: "allow", reason: "blacklisted" });
    expect(fetch.calls).toHaveLength(0);

    expect((await controller.handleDownload(browserDownload({ id: 9 }))).action).toBe("takeover");
    expect(fetch.calls).toHaveLength(1);
    expect(events).toEqual(["cancel:9", "erase:9"]);
  });
});

function popupState(state: unknown): Record<string, unknown> {
  return state as Record<string, unknown>;
}

function takeoverErrorOf(state: Record<string, unknown>): TakeoverFailure | null | undefined {
  return state.takeoverError as TakeoverFailure | null | undefined;
}

describe("takeover failure surface", () => {
  // AC-016: the popup exposes the last takeover failure from memory only.
  it("records a failed takeover in popup state without persisting it or leaking the token", async () => {
    const { controller, persistent, session } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      handler: async () => {
        throw new Error("offline");
      },
    });

    expect(await controller.handleDownload(browserDownload())).toEqual({
      action: "allow",
      reason: "capture_failed",
    });

    const state = popupState(
      await controller.handleRuntimeMessage(
        runtimeMessage({ type: "popup/get-state", tabId: 1 }),
        { tabId: 1 },
      ),
    );
    expect(takeoverErrorOf(state)).toEqual({
      downloadId: 7,
      url: "https://cdn.test/files/movie.mp4",
      filename: "movie.mp4",
      reason: "capture_failed",
    });
    expect(JSON.stringify(state)).not.toContain(TOKEN_A);
    expect(JSON.stringify(state)).not.toMatch(/[0-9a-f]{64}/i);

    expect(Object.keys(persistent.store).sort()).toEqual(["oneget.settings", "oneget.token"]);
    expect(JSON.stringify(persistent.store)).not.toContain("takeoverError");
    expect(JSON.stringify(persistent.store)).not.toContain("https://cdn.test/files/movie.mp4");
    expect(JSON.stringify(session.store)).not.toContain("takeoverError");
  });

  it("maps 401, 409, and 500 responses to the matching failure reason", async () => {
    const cases = [
      { status: 401, reason: "token_revoked" },
      { status: 409, reason: "update_required" },
      { status: 500, reason: "capture_failed" },
    ] as const;

    for (const entry of cases) {
      const { controller } = setUp({
        token: TOKEN_A,
        settings: savedSettings(),
        handler: () => jsonResponse({ code: "FAILED", message: "failed" }, entry.status),
      });

      expect(await controller.handleDownload(browserDownload())).toEqual({
        action: "allow",
        reason: entry.reason,
      });

      const state = popupState(
        await controller.handleRuntimeMessage(
          runtimeMessage({ type: "popup/get-state", tabId: 1 }),
          { tabId: 1 },
        ),
      );
      expect(takeoverErrorOf(state)).toMatchObject({
        downloadId: 7,
        url: "https://cdn.test/files/movie.mp4",
        filename: "movie.mp4",
        reason: entry.reason,
      });
    }
  });

  it("clears the recorded failure after a later successful takeover", async () => {
    let succeed = false;
    const { controller } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      handler: () =>
        succeed
          ? takeoverAccepted("task-1")
          : jsonResponse({ code: "INTERNAL", message: "boom" }, 500),
    });

    expect(await controller.handleDownload(browserDownload())).toEqual({
      action: "allow",
      reason: "capture_failed",
    });
    const failedState = popupState(
      await controller.handleRuntimeMessage(
        runtimeMessage({ type: "popup/get-state", tabId: 1 }),
        { tabId: 1 },
      ),
    );
    expect(takeoverErrorOf(failedState)).toMatchObject({ reason: "capture_failed" });

    succeed = true;
    expect(await controller.handleDownload(browserDownload({ id: 8 }))).toMatchObject({
      action: "takeover",
    });

    const recoveredState = popupState(
      await controller.handleRuntimeMessage(
        runtimeMessage({ type: "popup/get-state", tabId: 1 }),
        { tabId: 1 },
      ),
    );
    expect(takeoverErrorOf(recoveredState)).toBeNull();
  });

  it("clears the recorded failure after a successful prefill retry for the same URL", async () => {
    let calls = 0;
    const url = "https://cdn.test/files/retry.mp4";
    const { controller, fetch } = setUp({
      token: TOKEN_A,
      settings: savedSettings(),
      handler: () => {
        calls += 1;
        return calls === 1
          ? jsonResponse({ code: "INTERNAL", message: "boom" }, 500)
          : jsonResponse({ intent: "prefill", accepted: 1, rejected: [], duplicate: false });
      },
    });

    await controller.handleDownload(browserDownload({ url, filename: "retry.mp4" }));
    const failedState = popupState(
      await controller.handleRuntimeMessage(
        runtimeMessage({ type: "popup/get-state", tabId: 1 }),
        { tabId: 1 },
      ),
    );
    expect(takeoverErrorOf(failedState)).toMatchObject({
      url,
      reason: "capture_failed",
    });

    const retried = await controller.handleRuntimeMessage(
      runtimeMessage({
        type: "popup/capture-prefill",
        tabId: 1,
        url,
        resource: { url, filename: "retry.mp4", pageUrl: "https://page.test/watch" },
      }),
      { tabId: 1 },
    );
    expect(retried).toMatchObject({ ok: true });
    expect(fetch.calls).toHaveLength(2);

    const recoveredState = popupState(
      await controller.handleRuntimeMessage(
        runtimeMessage({ type: "popup/get-state", tabId: 1 }),
        { tabId: 1 },
      ),
    );
    expect(takeoverErrorOf(recoveredState)).toBeNull();
  });
});

describe("MV3 manifest", () => {
  it("declares a bundled MV3 service worker, popup, permissions, loopback host, CSP, content scripts, and icons", () => {
    const manifest = JSON.parse(
      readFileSync(new URL("../../chromium/manifest.json", import.meta.url), "utf8"),
    ) as Record<string, unknown>;

    expect(manifest.manifest_version).toBe(3);

    const background = manifest.background as Record<string, unknown> | undefined;
    expect(background).toMatchObject({ service_worker: "background.js", type: "module" });

    const action = manifest.action as Record<string, unknown> | undefined;
    expect(typeof action?.default_popup).toBe("string");
    expect((action?.default_popup as string).length).toBeGreaterThan(0);

    const permissions = (manifest.permissions ?? []) as string[];
    expect([...permissions].sort()).toEqual(
      ["downloads", "storage", "tabs", "webNavigation", "webRequest"].sort(),
    );
    expect(permissions).not.toContain("cookies");
    expect(permissions).not.toContain("nativeMessaging");

    const hosts = (manifest.host_permissions ?? []) as string[];
    expect(hosts).toEqual(
      expect.arrayContaining([
        "http://*/*",
        "https://*/*",
        "http://127.0.0.1:37652/*",
      ]),
    );
    expect(hosts).not.toContain("http://localhost/*");
    expect(hosts).not.toContain("<all_urls>");

    const csp = (manifest.content_security_policy as Record<string, unknown> | undefined)
      ?.extension_pages;
    expect(typeof csp).toBe("string");
    expect(csp as string).toContain("script-src 'self'");
    expect(csp as string).not.toMatch(/eval/i);
    expect(csp as string).not.toMatch(/script-src[^;]*(https?:|\*)/i);
    expect(csp as string).not.toMatch(/https:\/\//i);
    expect(csp as string).not.toMatch(/http:\/\/(?!127\.0\.0\.1:37652)/i);

    const scripts = (manifest.content_scripts ?? []) as Array<Record<string, unknown>>;
    expect(Array.isArray(manifest.content_scripts)).toBe(true);
    const matchesHttp = (script: Record<string, unknown>) =>
      Array.isArray(script.matches) &&
      (script.matches as string[]).some(
        (match) => match === "http://*/*" || match === "https://*/*",
      );
    const isolated = scripts.find((script) => matchesHttp(script) && script.world !== "MAIN");
    const main = scripts.find((script) => matchesHttp(script) && script.world === "MAIN");
    expect(isolated).toBeDefined();
    expect(main).toBeDefined();

    const icons = (manifest.icons ?? {}) as Record<string, unknown>;
    expect(icons).toBeTypeOf("object");
    expect(Object.keys(icons).length).toBeGreaterThan(0);
    for (const path of Object.values(icons)) {
      expect(typeof path).toBe("string");
      expect(path as string).not.toMatch(/^https?:/i);
    }

    const externallyConnectable = manifest.externally_connectable as
      | Record<string, unknown>
      | undefined;
    if (externallyConnectable !== undefined) {
      const matches = (externallyConnectable.matches ?? []) as string[];
      expect(matches).not.toContain("*://*/*");
      expect(matches).not.toContain("<all_urls>");
    }
  });
});




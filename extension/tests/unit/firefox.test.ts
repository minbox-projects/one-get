import { describe, expect, it, vi } from "vitest";

// Adapter boundary under test:
//   createFirefoxBackground({ browser, fetchImpl, apiOrigin, now }) installs the
//   promise-style Firefox event listeners and returns the shared controller.
//   installFirefoxBackground() is the guarded automatic entry point.
type FirefoxModule = typeof import("../../firefox/background");

const TEST_ORIGIN = "http://127.0.0.1:37652";
const TOKEN_A = "0123456789abcdef".repeat(4);
const TOKEN_B = "fedcba9876543210".repeat(4);

type Listener = (...args: unknown[]) => unknown;

async function loadAdapter(): Promise<FirefoxModule> {
  // Kept dynamic so every behavior test reports its own module boundary failure.
  return await vi.importActual<FirefoxModule>("../../firefox/background");
}

interface Area {
  store: Record<string, unknown>;
  get(keys: string | string[] | null): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
  remove(keys: string | string[]): Promise<void>;
}

function createArea(initial: Record<string, unknown> = {}): Area {
  const store: Record<string, unknown> = { ...initial };
  return {
    store,
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
}

function settingsDocument(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    schemaVersion: 2,
    takeoverEnabled: true,
    minimumSizeBytes: 1024,
    blacklistHosts: [],
    categories: {
      video: true,
      audio: true,
      archive: true,
      program: true,
      document: true,
      model: true,
      other: true,
    },
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
  body: unknown;
}

type FetchHandler = (url: string) => Response | Promise<Response>;

function createFetch(handler: FetchHandler) {
  const calls: FetchCall[] = [];
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const text = typeof init?.body === "string" ? init.body : undefined;
    calls.push({
      url,
      body: text === undefined ? undefined : (JSON.parse(text) as unknown),
    });
    return handler(url);
  }) as typeof fetch;
  return { fetchImpl, calls };
}

interface MockOptions {
  token?: string;
  settings?: Record<string, unknown>;
  session?: Record<string, unknown>;
  cancelError?: Error;
}

function firefoxMock(options: MockOptions = {}) {
  const local = createArea({
    ...(options.token === undefined ? {} : { "oneget.token": options.token }),
    ...(options.settings === undefined ? {} : { "oneget.settings": options.settings }),
  });
  const session = createArea(options.session ?? {});
  const events: string[] = [];
  const determiningFilenameReads: string[] = [];
  const listeners = {
    message: [] as Listener[],
    responseStarted: [] as Listener[],
    downloadCreated: [] as Listener[],
    committed: [] as Listener[],
    historyStateUpdated: [] as Listener[],
    tabRemoved: [] as Listener[],
  };

  const downloads: Record<string, unknown> = {
    onCreated: { addListener: (listener: Listener) => listeners.downloadCreated.push(listener) },
    cancel: async (id: number) => {
      events.push(`cancel:${id}`);
      if (options.cancelError !== undefined) {
        throw options.cancelError;
      }
    },
    erase: async (query: { id: number }) => {
      events.push(`erase:${query.id}`);
    },
  };
  Object.defineProperty(downloads, "onDeterminingFilename", {
    get() {
      determiningFilenameReads.push("onDeterminingFilename");
      return undefined;
    },
  });

  const browser = {
    runtime: {
      sendMessage: async () => undefined,
      onMessage: {
        addListener: (listener: Listener) => listeners.message.push(listener),
      },
    },
    storage: { local, session },
    tabs: {
      query: async () => [{ id: 5, url: "https://page.test/watch" }],
      onRemoved: { addListener: (listener: Listener) => listeners.tabRemoved.push(listener) },
    },
    webRequest: {
      onResponseStarted: {
        addListener: (listener: Listener) => listeners.responseStarted.push(listener),
      },
    },
    downloads,
    webNavigation: {
      onCommitted: { addListener: (listener: Listener) => listeners.committed.push(listener) },
      onHistoryStateUpdated: {
        addListener: (listener: Listener) => listeners.historyStateUpdated.push(listener),
      },
    },
  };

  return { browser, listeners, local, session, events, determiningFilenameReads };
}

interface InstallOptions {
  fetchImpl?: typeof fetch;
}

async function install(mock: ReturnType<typeof firefoxMock>, options: InstallOptions = {}) {
  const module = await loadAdapter();
  const factory = module.createFirefoxBackground as unknown as (deps: {
    browser: unknown;
    fetchImpl?: typeof fetch;
    apiOrigin: string;
    now: () => number;
  }) => unknown;
  factory({
    browser: mock.browser,
    fetchImpl: options.fetchImpl,
    apiOrigin: TEST_ORIGIN,
    now: () => 1_000,
  });
  return module;
}

function latest(listeners: Listener[]): Listener {
  const listener = listeners[listeners.length - 1];
  expect(listener).toBeDefined();
  return listener;
}

async function sendMessage(
  mock: ReturnType<typeof firefoxMock>,
  message: Record<string, unknown>,
  sender: Record<string, unknown> = { tab: { id: 1 }, frameId: 0 },
): Promise<unknown> {
  return await Promise.resolve(latest(mock.listeners.message)(message, sender));
}

async function popupState(mock: ReturnType<typeof firefoxMock>, tabId = 1) {
  return (await sendMessage(mock, { type: "popup/get-state", tabId }, { tabId })) as Record<
    string,
    unknown
  >;
}

function firefoxDownload(overrides: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: 7,
    url: "https://cdn.test/files/movie.mp4",
    filename: "/home/user/Downloads/movie.mp4",
    referrer: "https://page.test/watch",
    mime: "video/mp4",
    totalBytes: 2 * 1024 * 1024,
    incognito: false,
    byExtensionId: "",
    ...overrides,
  };
}

async function flush(): Promise<void> {
  for (let round = 0; round < 3; round += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function created(mock: ReturnType<typeof firefoxMock>, item: Record<string, unknown>) {
  latest(mock.listeners.downloadCreated)(item);
  await flush();
}

describe("Firefox background installation", () => {
  it("registers runtime, response, download, navigation, SPA, and tab listeners synchronously", async () => {
    const mock = firefoxMock();

    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });

    expect(mock.listeners.message).toHaveLength(1);
    expect(mock.listeners.responseStarted).toHaveLength(1);
    expect(mock.listeners.downloadCreated).toHaveLength(1);
    expect(mock.listeners.committed).toHaveLength(1);
    expect(mock.listeners.historyStateUpdated).toHaveLength(1);
    expect(mock.listeners.tabRemoved).toHaveLength(1);
    expect(mock.determiningFilenameReads).toEqual([]);
  });

  it("does not throw or install when the browser global is unavailable", async () => {
    expect((globalThis as Record<string, unknown>).browser).toBeUndefined();
    const module = await loadAdapter();
    const installAuto = module.installFirefoxBackground as unknown as () => void;

    expect(() => installAuto()).not.toThrow();
  });
});

describe("Firefox download mapping", () => {
  it("maps an absolute local filename to a safe basename and forwards download metadata", async () => {
    const fetch = createFetch(() => takeoverAccepted("task-42"));
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(mock, firefoxDownload());

    await vi.waitFor(() => expect(fetch.calls).toHaveLength(1));
    const body = fetch.calls[0].body as Record<string, unknown>;
    const items = body.items as Array<Record<string, unknown>>;
    expect(body).toMatchObject({ protocolVersion: 1, intent: "takeover" });
    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({
      url: "https://cdn.test/files/movie.mp4",
      filename: "movie.mp4",
      referer: "https://page.test/watch",
      mime: "video/mp4",
      size: 2 * 1024 * 1024,
      kind: "file",
    });
  });

  it("derives a safe filename from the URL when the download has no filename", async () => {
    const fetch = createFetch(() => takeoverAccepted("task-clip"));
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(
      mock,
      firefoxDownload({
        id: 8,
        url: "https://cdn.test/files/clip.mp4",
        filename: "",
      }),
    );

    await vi.waitFor(() => expect(fetch.calls).toHaveLength(1));
    const items = (fetch.calls[0].body as Record<string, unknown>).items as Array<
      Record<string, unknown>
    >;
    expect(items[0]).toMatchObject({ url: "https://cdn.test/files/clip.mp4", filename: "clip.mp4" });
  });

  it("maps a -1 total size to unknown and skips the minimum-size rule", async () => {
    const fetch = createFetch(() => takeoverAccepted("task-unknown"));
    const mock = firefoxMock({
      token: TOKEN_A,
      settings: settingsDocument({ minimumSizeBytes: 100 * 1024 * 1024 }),
    });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(mock, firefoxDownload({ totalBytes: -1 }));

    await vi.waitFor(() => expect(fetch.calls).toHaveLength(1));
    const items = (fetch.calls[0].body as Record<string, unknown>).items as Array<
      Record<string, unknown>
    >;
    expect(items[0].size === undefined || items[0].size === null).toBe(true);
    expect(mock.events).toEqual(["cancel:7", "erase:7"]);
  });

  it("does not take over non-HTTP, manifest, incognito, or extension-originated downloads", async () => {
    const cases: Array<Record<string, unknown>> = [
      { url: "ftp://cdn.test/files/movie.mp4" },
      { url: "https://cdn.test/live/master.m3u8", filename: "master.m3u8" },
      { url: "https://cdn.test/stream/manifest.mpd", filename: "manifest.mpd" },
      { incognito: true },
      { byExtensionId: "addon@oneget.test" },
    ];

    for (const overrides of cases) {
      const fetch = createFetch(() => takeoverAccepted());
      const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
      await install(mock, { fetchImpl: fetch.fetchImpl });

      await created(mock, firefoxDownload(overrides));

      expect(fetch.calls, JSON.stringify(overrides)).toHaveLength(0);
      expect(mock.events, JSON.stringify(overrides)).toEqual([]);
    }
  });
});

describe("Firefox takeover orchestration", () => {
  it("sends exactly one capture before canceling and erasing the browser download", async () => {
    const events: string[] = [];
    const fetch = createFetch(() => {
      events.push("capture");
      return takeoverAccepted("task-7");
    });
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(mock, firefoxDownload());

    await vi.waitFor(() => expect(mock.events).toContain("erase:7"));
    expect(fetch.calls).toHaveLength(1);
    expect(events).toEqual(["capture"]);
    expect(mock.events).toEqual(["cancel:7", "erase:7"]);
  });

  it("still erases exactly once when the browser cancel rejects after a successful capture", async () => {
    const debug = vi.spyOn(console, "debug").mockImplementation(() => {});
    try {
      let captures = 0;
      const fetch = createFetch(() => {
        captures += 1;
        return takeoverAccepted("task-7");
      });
      const mock = firefoxMock({
        token: TOKEN_A,
        settings: settingsDocument(),
        cancelError: new Error("download already completed"),
      });
      await install(mock, { fetchImpl: fetch.fetchImpl });

      await created(mock, firefoxDownload());

      await vi.waitFor(() => expect(mock.events).toContain("erase:7"));
      expect(captures).toBe(1);
      expect(fetch.calls).toHaveLength(1);
      expect(mock.events.filter((event) => event === "cancel:7")).toHaveLength(1);
      expect(mock.events.filter((event) => event === "erase:7")).toHaveLength(1);
      expect((await popupState(mock)).takeoverError ?? null).toBeNull();
      const cancelFailureLogged = debug.mock.calls.some(([prefix, event]) => {
        return (
          prefix === "[oneget]" &&
          typeof event === "string" &&
          event.startsWith("download.") &&
          (event.includes("cancel") || event.includes("cleanup"))
        );
      });
      expect(cancelFailureLogged).toBe(true);

      await created(mock, firefoxDownload());
      expect(captures).toBe(1);
      expect(fetch.calls).toHaveLength(1);
      expect(mock.events.filter((event) => event === "erase:7")).toHaveLength(1);
    } finally {
      debug.mockRestore();
    }
  });

  it("leaves the browser download intact when the capture fails", async () => {
    const failures: Array<{ name: string; handler: FetchHandler }> = [
      {
        name: "a network error",
        handler: () => {
          throw new Error("offline");
        },
      },
      {
        name: "a server error",
        handler: () => jsonResponse({ code: "INTERNAL", message: "boom" }, 500),
      },
    ];

    for (const failure of failures) {
      const fetch = createFetch(failure.handler);
      const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
      await install(mock, { fetchImpl: fetch.fetchImpl });

      await created(mock, firefoxDownload());

      expect(fetch.calls, failure.name).toHaveLength(1);
      expect(mock.events, failure.name).toEqual([]);
    }
  });

  it("clears the token on 401 and keeps the token_revoked retry record", async () => {
    const failure = createFetch(() => jsonResponse({ code: "UNAUTHORIZED", message: "revoked" }, 401));
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: failure.fetchImpl });

    await created(mock, firefoxDownload());

    await vi.waitFor(() => expect(mock.local.store["oneget.token"]).toBeUndefined());
    const state = await popupState(mock);
    expect(state.takeoverError).toMatchObject({
      downloadId: 7,
      url: "https://cdn.test/files/movie.mp4",
      filename: "movie.mp4",
      reason: "token_revoked",
    });
    expect(mock.events).toEqual([]);
  });

  it("records update_required on 409 and leaves the browser download intact", async () => {
    const failure = createFetch(() =>
      jsonResponse({ code: "UPDATE_REQUIRED", message: "update required" }, 409),
    );
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: failure.fetchImpl });

    await created(mock, firefoxDownload());

    await vi.waitFor(async () => {
      const state = await popupState(mock);
      expect(state.takeoverError).toMatchObject({ reason: "update_required" });
    });
    expect(mock.events).toEqual([]);
    expect(mock.local.store["oneget.token"]).toBe(TOKEN_A);
  });

  it("does not repeat a capture or cleanup for a duplicate token-bound source", async () => {
    let calls = 0;
    const fetch = createFetch(() => {
      calls += 1;
      return takeoverAccepted("task-9");
    });
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(mock, firefoxDownload());
    await vi.waitFor(() => expect(mock.events).toContain("erase:7"));
    await created(mock, firefoxDownload());

    expect(calls).toBe(1);
    expect(mock.events).toEqual(["cancel:7", "erase:7"]);
  });

  it("does not cancel or erase when the desktop reports a duplicate token-bound source", async () => {
    const fetch = createFetch(() => takeoverAccepted("task-existing", true));
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: fetch.fetchImpl });

    await created(mock, firefoxDownload());

    expect(fetch.calls).toHaveLength(1);
    expect(mock.events).toEqual([]);
  });

  it("leaves unpaired, disabled, blacklisted, and small known-size downloads untouched", async () => {
    const cases: Array<{
      name: string;
      token?: string;
      settings?: Record<string, unknown>;
      download?: Record<string, unknown>;
    }> = [
      { name: "unpaired", settings: settingsDocument() },
      {
        name: "disabled",
        token: TOKEN_A,
        settings: settingsDocument({ takeoverEnabled: false }),
      },
      {
        name: "blacklisted",
        token: TOKEN_A,
        settings: settingsDocument({ blacklistHosts: ["blocked.test"] }),
        download: { url: "https://cdn.blocked.test/movie.mp4" },
      },
      {
        name: "small known size",
        token: TOKEN_A,
        settings: settingsDocument({ minimumSizeBytes: 1024 * 1024 }),
        download: { totalBytes: 1024 },
      },
    ];

    for (const entry of cases) {
      const fetch = createFetch(() => takeoverAccepted());
      const mock = firefoxMock({ token: entry.token, settings: entry.settings });
      await install(mock, { fetchImpl: fetch.fetchImpl });

      await created(mock, firefoxDownload(entry.download));

      expect(fetch.calls, entry.name).toHaveLength(0);
      expect(mock.events, entry.name).toEqual([]);
    }
  });
});

describe("Firefox pairing", () => {
  it("pairs with the Firefox extension label without changing protocol strings", async () => {
    const fetch = createFetch(() =>
      jsonResponse({
        token: TOKEN_B,
        client: { id: "client-fx", label: "Firefox extension" },
      }),
    );
    const mock = firefoxMock();
    await install(mock, { fetchImpl: fetch.fetchImpl });

    const result = await sendMessage(mock, { type: "popup/pair", code: "12345678" });

    expect(result).toMatchObject({ ok: true });
    expect(fetch.calls).toHaveLength(1);
    expect(fetch.calls[0].url).toBe(`${TEST_ORIGIN}/pair`);
    expect(fetch.calls[0].body).toMatchObject({
      protocolVersion: 1,
      code: "12345678",
      label: "Firefox extension",
    });
    expect(mock.local.store["oneget.token"]).toBe(TOKEN_B);
  });
});

describe("Firefox event-page lifecycle", () => {
  it("rehydrates session resources after a suspension without moving them to local storage", async () => {
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });

    latest(mock.listeners.responseStarted)({
      requestId: "1",
      tabId: 3,
      frameId: 0,
      url: "https://cdn.test/files/movie.mp4",
      responseHeaders: [
        { name: "Content-Type", value: "video/mp4" },
        { name: "Content-Length", value: "2048" },
      ],
    });

    await vi.waitFor(() => {
      expect(JSON.stringify(mock.session.store)).toContain("https://cdn.test/files/movie.mp4");
    });

    const first = await popupState(mock, 3);
    expect(first.resources).toHaveLength(1);
    expect(JSON.stringify(mock.local.store)).not.toContain("https://cdn.test/files/movie.mp4");

    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });
    await flush();
    const afterSuspension = await popupState(mock, 3);
    expect(afterSuspension.resources).toMatchObject([
      { url: "https://cdn.test/files/movie.mp4", mime: "video/mp4", size: 2048 },
    ]);
  });

  it("rehydrates the bounded takeover failure so the popup still offers a retry", async () => {
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, {
      fetchImpl: createFetch(() => jsonResponse({ code: "INTERNAL", message: "boom" }, 500))
        .fetchImpl,
    });

    await created(mock, firefoxDownload());
    await vi.waitFor(() => {
      expect(mock.session.store["oneget.takeover-error"]).toMatchObject({
        downloadId: 7,
        url: "https://cdn.test/files/movie.mp4",
        filename: "movie.mp4",
        reason: "capture_failed",
      });
    });
    expect(JSON.stringify(mock.local.store)).not.toContain("capture_failed");

    await install(mock, {
      fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl,
    });
    await flush();
    const afterSuspension = await popupState(mock);
    expect(afterSuspension.takeoverError).toMatchObject({
      downloadId: 7,
      url: "https://cdn.test/files/movie.mp4",
      reason: "capture_failed",
    });
  });

  it("routes SPA and tab-close events to the shared resource store", async () => {
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });

    latest(mock.listeners.responseStarted)({
      tabId: 4,
      frameId: 0,
      url: "https://cdn.test/files/spa.mp4",
      responseHeaders: [{ name: "Content-Type", value: "video/mp4" }],
    });
    await vi.waitFor(async () => {
      expect((await popupState(mock, 4)).resources).toHaveLength(1);
    });

    latest(mock.listeners.historyStateUpdated)({ tabId: 4, frameId: 0, url: "https://page.test/next" });
    await vi.waitFor(async () => {
      expect((await popupState(mock, 4)).resources).toEqual([]);
    });

    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });
    await sendMessage(
      mock,
      { type: "content/resource", url: "https://cdn.test/files/closing.mp4", pageUrl: "https://page.test/watch" },
      { tab: { id: 6 }, frameId: 0 },
    );
    await vi.waitFor(async () => {
      expect((await popupState(mock, 6)).resources).toHaveLength(1);
    });
    latest(mock.listeners.tabRemoved)(6);
    await vi.waitFor(async () => {
      expect((await popupState(mock, 6)).resources).toEqual([]);
    });
  });

  it("keeps the token and settings in local storage", async () => {
    const mock = firefoxMock({ token: TOKEN_A, settings: settingsDocument() });
    await install(mock, { fetchImpl: createFetch(() => takeoverAccepted()).fetchImpl });

    await sendMessage(mock, {
      type: "settings/changed",
      settings: settingsDocument({ takeoverEnabled: false }),
    });

    expect(mock.local.store["oneget.token"]).toBe(TOKEN_A);
    expect(mock.local.store["oneget.settings"]).toMatchObject({ takeoverEnabled: false });
    expect(JSON.stringify(mock.session.store)).not.toContain(TOKEN_A);
    expect(JSON.stringify(mock.session.store)).not.toContain("oneget.token");
  });
});

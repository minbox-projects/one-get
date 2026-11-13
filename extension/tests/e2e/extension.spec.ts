import { chromium, expect, test } from "@playwright/test";
import type { BrowserContext, Page, Worker } from "@playwright/test";
import { existsSync } from "node:fs";
import { mkdir, readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import type { IncomingHttpHeaders, IncomingMessage, Server, ServerResponse } from "node:http";
import path from "node:path";

const FIXTURE_PORT = 37654;
const API_PORT = 37653;
const FIXTURE_ORIGIN = `http://127.0.0.1:${FIXTURE_PORT}`;
const API_ORIGIN = `http://127.0.0.1:${API_PORT}`;
const PAIRING_CODE = "12345678";
const PAIRING_TOKEN = "0123456789abcdef".repeat(4);
const PROJECT_ROOT = process.cwd();
const FIXTURES_DIR = path.join(PROJECT_ROOT, "extension", "tests", "e2e", "fixtures");
const EXTENSION_PATH = path.join(PROJECT_ROOT, "extension", "dist", "chromium-test");
const PROFILE_DIR = path.join(PROJECT_ROOT, "test-results", "extension-e2e-profile");
const OVERLAY_BUTTON = ".oneget-overlay-button";
const OVERLAY_NOTICE = ".oneget-overlay-notice";

interface FixtureFiles {
  pageHtml: Buffer;
  masterManifest: Buffer;
  segment1: Buffer;
  segment2: Buffer;
  downloadZip: Buffer;
}

interface RecordedRequest {
  method: string;
  path: string;
  headers: IncomingHttpHeaders;
  body: string;
}

interface CaptureItemRecord {
  url?: string;
  kind?: string;
  headers?: Array<{ name?: string; value?: string }>;
}

interface CaptureBody {
  intent?: string;
  sourceId?: string;
  protocolVersion?: number;
  items?: CaptureItemRecord[];
}

interface RecordedCapture {
  headers: IncomingHttpHeaders;
  body: CaptureBody;
}

interface ChromeRuntime {
  runtime: { sendMessage(message: unknown): Promise<unknown> };
  tabs: { query(queryInfo?: Record<string, unknown>): Promise<Array<{ id?: number; url?: string }>> };
  downloads?: {
    onDeterminingFilename?: { addListener?: unknown };
    search(query: Record<string, unknown>): Promise<Array<{ id: number; url?: string }>>;
  };
}

let fixtureServer: Server | null = null;
let apiServer: Server | null = null;
let context: BrowserContext | null = null;
let fixturePage: Page | null = null;
let popupPage: Page | null = null;
let worker: Worker | null = null;
let extensionId = "";
let fixtureTabId = -1;
const pairRequests: RecordedRequest[] = [];
const captures: RecordedCapture[] = [];

function requirePage(page: Page | null, label: string): Page {
  if (page === null) {
    throw new Error(`${label} page is not open`);
  }
  return page;
}

function requireWorker(): Worker {
  if (worker === null) {
    throw new Error("extension service worker is not available");
  }
  return worker;
}

function requireTabId(): number {
  if (fixtureTabId < 0) {
    throw new Error("fixture tab id has not been resolved");
  }
  return fixtureTabId;
}

function extensionOrigin(): string {
  return `chrome-extension://${extensionId}`;
}

function listen(server: Server, port: number, label: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const onError = (error: Error) => {
      server.off("listening", onListening);
      reject(new Error(`${label} could not bind 127.0.0.1:${port}: ${error.message}`));
    };
    const onListening = () => {
      server.off("error", onError);
      resolve();
    };
    server.once("error", onError);
    server.once("listening", onListening);
    server.listen(port, "127.0.0.1");
  });
}

function closeServer(server: Server): Promise<void> {
  return new Promise((resolve) => {
    server.close(() => resolve());
    server.closeAllConnections();
  });
}

function sendJson(
  response: ServerResponse,
  status: number,
  value: unknown,
  origin?: string,
): void {
  const headers: Record<string, string> = {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
  };
  if (origin !== undefined && origin.startsWith("chrome-extension://")) {
    headers["access-control-allow-origin"] = origin;
    headers["access-control-allow-headers"] = "authorization, content-type";
    headers["access-control-allow-methods"] = "GET, POST, OPTIONS";
    headers.vary = "Origin";
  }
  response.writeHead(status, headers);
  response.end(JSON.stringify(value));
}

function readRequestBody(request: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk) => {
      chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
    });
    request.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    request.on("error", reject);
  });
}

function headerValue(headers: IncomingHttpHeaders, name: string): string | undefined {
  const value = headers[name];
  if (Array.isArray(value)) {
    return value[0];
  }
  return value;
}

function serveFixture(
  request: IncomingMessage,
  response: ServerResponse,
  files: FixtureFiles,
): void {
  const pathname = new URL(request.url ?? "/", FIXTURE_ORIGIN).pathname;
  const send = (
    status: number,
    contentType: string,
    body: Buffer,
    extra: Record<string, string> = {},
  ): void => {
    response.writeHead(status, {
      "content-type": contentType,
      "content-length": String(body.byteLength),
      "cache-control": "no-store",
      ...extra,
    });
    response.end(body);
  };

  switch (pathname) {
    case "/":
    case "/page.html":
      send(200, "text/html; charset=utf-8", files.pageHtml);
      return;
    case "/hls/master.m3u8":
      send(200, "application/vnd.apple.mpegurl", files.masterManifest);
      return;
    case "/hls/segment1.ts":
      send(200, "video/mp2t", files.segment1);
      return;
    case "/hls/segment2.ts":
      send(200, "video/mp2t", files.segment2);
      return;
    case "/download.zip":
    case "/spa/second-resource.zip":
      send(200, "application/zip", files.downloadZip);
      return;
    case "/media/sample.mp4":
      send(200, "video/mp4", Buffer.from("oneget-video-fixture"));
      return;
    case "/takeover.bin":
      send(200, "application/octet-stream", files.downloadZip, {
        "content-disposition": 'attachment; filename="takeover.bin"',
      });
      return;
    default:
      send(404, "text/plain; charset=utf-8", Buffer.from("not found"));
  }
}

async function handleApiRequest(
  request: IncomingMessage,
  response: ServerResponse,
): Promise<void> {
  const origin = headerValue(request.headers, "origin");
  const pathname = new URL(request.url ?? "/", API_ORIGIN).pathname;

  if (request.method === "OPTIONS") {
    response.writeHead(204, {
      "access-control-allow-origin": origin ?? "",
      "access-control-allow-headers": "authorization, content-type",
      "access-control-allow-methods": "GET, POST, OPTIONS",
      "cache-control": "no-store",
      vary: "Origin",
    });
    response.end();
    return;
  }

  if (request.method === "GET" && pathname === "/health") {
    sendJson(
      response,
      200,
      {
        ok: true,
        appVersion: "0.1.0",
        protocolVersion: 1,
        capabilities: ["prefill", "takeover"],
      },
      origin,
    );
    return;
  }

  if (request.method !== "POST") {
    sendJson(response, 405, { code: "METHOD_NOT_ALLOWED", message: "unsupported method" }, origin);
    return;
  }

  const rawBody = await readRequestBody(request);

  if (pathname === "/pair") {
    pairRequests.push({ method: "POST", path: pathname, headers: request.headers, body: rawBody });
    if (!origin?.startsWith("chrome-extension://")) {
      sendJson(
        response,
        403,
        { code: "ORIGIN_REQUIRED", message: "a Chromium extension origin is required" },
        origin,
      );
      return;
    }

    let parsed: { code?: unknown; protocolVersion?: unknown };
    try {
      parsed = JSON.parse(rawBody) as { code?: unknown; protocolVersion?: unknown };
    } catch {
      sendJson(response, 400, { code: "INVALID_JSON", message: "invalid JSON" }, origin);
      return;
    }
    if (parsed.protocolVersion !== 1) {
      sendJson(response, 409, { code: "PROTOCOL_MISMATCH", message: "protocol mismatch" }, origin);
      return;
    }
    if (parsed.code !== PAIRING_CODE) {
      sendJson(response, 401, { code: "INVALID_CODE", message: "invalid pairing code" }, origin);
      return;
    }

    sendJson(
      response,
      200,
      {
        protocolVersion: 1,
        token: PAIRING_TOKEN,
        client: { id: "client-1", label: "Chromium test extension", origin },
      },
      origin,
    );
    return;
  }

  if (pathname === "/capture") {
    if (!origin?.startsWith("chrome-extension://")) {
      sendJson(
        response,
        403,
        { code: "ORIGIN_REQUIRED", message: "a Chromium extension origin is required" },
        origin,
      );
      return;
    }
    if (headerValue(request.headers, "authorization") !== `Bearer ${PAIRING_TOKEN}`) {
      sendJson(response, 401, { code: "UNAUTHORIZED", message: "invalid token" }, origin);
      return;
    }

    let parsed: CaptureBody;
    try {
      parsed = JSON.parse(rawBody) as CaptureBody;
    } catch {
      sendJson(response, 400, { code: "INVALID_JSON", message: "invalid JSON" }, origin);
      return;
    }
    if (parsed.protocolVersion !== 1) {
      sendJson(response, 409, { code: "PROTOCOL_MISMATCH", message: "protocol mismatch" }, origin);
      return;
    }

    captures.push({ headers: request.headers, body: parsed });
    sendJson(
      response,
      200,
      {
        protocolVersion: 1,
        intent: parsed.intent,
        accepted: parsed.items?.length ?? 0,
        rejected: [],
        taskId: `task-${captures.length}`,
        duplicate: false,
      },
      origin,
    );
    return;
  }

  if (pathname === "/pair/revoke") {
    if (headerValue(request.headers, "authorization") !== `Bearer ${PAIRING_TOKEN}`) {
      sendJson(response, 401, { code: "UNAUTHORIZED", message: "invalid token" }, origin);
      return;
    }
    sendJson(response, 200, { revoked: true }, origin);
    return;
  }

  sendJson(response, 404, { code: "NOT_FOUND", message: "unknown route" }, origin);
}

async function firstServiceWorker(browserContext: BrowserContext): Promise<Worker> {
  const existing = browserContext.serviceWorkers();
  if (existing.length > 0) {
    return existing[0];
  }
  return browserContext.waitForEvent("serviceworker");
}

function serviceWorkerExtensionId(serviceWorker: Worker): string {
  const parsed = new URL(serviceWorker.url());
  if (parsed.protocol !== "chrome-extension:") {
    throw new Error(`unexpected service worker URL: ${serviceWorker.url()}`);
  }
  return parsed.host;
}

async function resolveFixtureTabId(serviceWorker: Worker, origin: string): Promise<number> {
  return serviceWorker.evaluate(async (targetOrigin) => {
    const chromeApi = (globalThis as unknown as { chrome: ChromeRuntime }).chrome;
    const tabs = await chromeApi.tabs.query({});
    const tab = tabs.find(
      (candidate) => typeof candidate.url === "string" && candidate.url.startsWith(targetOrigin),
    );
    if (tab?.id === undefined) {
      throw new Error(`no browser tab matched ${targetOrigin}`);
    }
    return tab.id;
  }, origin);
}

async function pinActiveTab(page: Page, tabId: number): Promise<void> {
  await page.addInitScript((activeTabId) => {
    const chromeApi = (globalThis as unknown as { chrome: ChromeRuntime }).chrome;
    const originalQuery = chromeApi.tabs.query.bind(chromeApi.tabs);
    chromeApi.tabs.query = (queryInfo?: Record<string, unknown>) => {
      if (queryInfo?.active === true && queryInfo.currentWindow === true) {
        return Promise.resolve([{ id: activeTabId, active: true }]);
      }
      return originalQuery(queryInfo);
    };
  }, tabId);
}

async function reloadPopup(): Promise<Page> {
  const popup = requirePage(popupPage, "popup");
  await popup.reload({ waitUntil: "load" });
  return popup;
}

async function listResourceUrls(tabId: number): Promise<string[]> {
  const popup = requirePage(popupPage, "popup");
  return popup.evaluate(async (activeTabId) => {
    const chromeApi = (globalThis as unknown as { chrome: ChromeRuntime }).chrome;
    const result = await chromeApi.runtime.sendMessage({ type: "resources:list", tabId: activeTabId });
    if (!Array.isArray(result)) {
      return [];
    }
    return result.flatMap((entry) => {
      if (typeof entry !== "object" || entry === null) {
        return [];
      }
      const url = (entry as { url?: unknown }).url;
      return typeof url === "string" ? [url] : [];
    });
  }, tabId);
}

async function waitForResource(tabId: number, suffix: string): Promise<void> {
  await expect
    .poll(async () => (await listResourceUrls(tabId)).some((url) => url.endsWith(suffix)), {
      timeout: 15_000,
    })
    .toBe(true);
}

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  if (!existsSync(path.join(EXTENSION_PATH, "manifest.json"))) {
    throw new Error(
      `missing extension test build at ${EXTENSION_PATH}; run npm run build:extension:test first`,
    );
  }

  const files: FixtureFiles = {
    pageHtml: await readFile(path.join(FIXTURES_DIR, "page.html")),
    masterManifest: await readFile(path.join(FIXTURES_DIR, "hls", "master.m3u8")),
    segment1: await readFile(path.join(FIXTURES_DIR, "hls", "segment1.ts")),
    segment2: await readFile(path.join(FIXTURES_DIR, "hls", "segment2.ts")),
    downloadZip: await readFile(path.join(FIXTURES_DIR, "download.zip")),
  };

  fixtureServer = createServer((request, response) => {
    serveFixture(request, response, files);
  });
  await listen(fixtureServer, FIXTURE_PORT, "fixture server");

  apiServer = createServer((request, response) => {
    void handleApiRequest(request, response).catch((error: unknown) => {
      sendJson(response, 500, { code: "MOCK_FAILURE", message: String(error) });
    });
  });
  await listen(apiServer, API_PORT, "mock API server");

  await rm(PROFILE_DIR, { recursive: true, force: true });
  await mkdir(PROFILE_DIR, { recursive: true });

  context = await chromium.launchPersistentContext(PROFILE_DIR, {
    channel: "chromium",
    headless: true,
    locale: "en-US",
    acceptDownloads: true,
    args: [
      `--disable-extensions-except=${EXTENSION_PATH}`,
      `--load-extension=${EXTENSION_PATH}`,
    ],
  });

  worker = await firstServiceWorker(context);
  extensionId = serviceWorkerExtensionId(worker);

  fixturePage = await context.newPage();
  await fixturePage.goto(`${FIXTURE_ORIGIN}/`, { waitUntil: "load" });
  await fixturePage.waitForFunction(
    () => (window as { __oneGetFixtureInitialLoaded?: boolean }).__oneGetFixtureInitialLoaded === true,
  );

  fixtureTabId = await resolveFixtureTabId(worker, FIXTURE_ORIGIN);

  popupPage = await context.newPage();
  await pinActiveTab(popupPage, fixtureTabId);
  await popupPage.goto(`chrome-extension://${extensionId}/popup.html`, { waitUntil: "load" });
});

test.beforeEach(() => {
  captures.length = 0;
  pairRequests.length = 0;
});

test.afterAll(async () => {
  if (context !== null) {
    await context.close().catch(() => undefined);
  }
  if (fixtureServer !== null) {
    await closeServer(fixtureServer);
  }
  if (apiServer !== null) {
    await closeServer(apiServer);
  }
});

test("loads the extension and opens the localized popup surfaces", async () => {
  expect(extensionId.length).toBeGreaterThan(0);
  expect(requireWorker().url()).toContain(`chrome-extension://${extensionId}/`);
  expect(requireWorker().url()).toContain("background.js");

  const popup = requirePage(popupPage, "popup");
  await expect(popup.locator("#resources-heading")).toHaveText("Resources");
  await expect(popup.locator("#pairing-heading")).toHaveText("Pairing");
  await expect(popup.locator("#pairing-status")).toHaveText("Not paired");
  await expect(popup.locator("#settings-heading")).toHaveText("Takeover settings");
  await expect(popup.locator("#settings-form button[type=submit]")).toHaveText("Save");
});

test("pairs from the popup with the exact Chromium extension origin", async () => {
  const popup = await reloadPopup();
  await expect(popup.locator("#pairing-status")).toHaveText("Not paired");

  await popup.locator("#pairing-code").fill(PAIRING_CODE);
  await popup.locator("#pair-form button[type=submit]").click();

  await expect(popup.locator("#pairing-status")).toHaveText("Paired with OneGet");
  await expect(popup.locator("#unpair-button")).toBeVisible();
  await expect(popup.locator("#pair-form")).toBeHidden();
  await expect.poll(() => pairRequests.length, { timeout: 10_000 }).toBe(1);

  const request = pairRequests[0];
  expect(request.method).toBe("POST");
  expect(request.path).toBe("/pair");
  expect(request.headers.origin).toBe(extensionOrigin());
  expect(JSON.parse(request.body)).toMatchObject({
    protocolVersion: 1,
    code: PAIRING_CODE,
  });
  expect(await popup.locator("body").innerText()).not.toContain(PAIRING_TOKEN);
});

test("lists the HLS manifest and archive while filtering transport segments", async () => {
  const fixture = requirePage(fixturePage, "fixture");
  await fixture.goto(`${FIXTURE_ORIGIN}/`, { waitUntil: "load" });
  await fixture.waitForFunction(
    () => (window as { __oneGetFixtureInitialLoaded?: boolean }).__oneGetFixtureInitialLoaded === true,
  );

  const tabId = requireTabId();
  await waitForResource(tabId, "/hls/master.m3u8");
  await waitForResource(tabId, "/download.zip");

  const popup = await reloadPopup();
  const manifestRow = popup.locator("#resources-list li", { hasText: "master.m3u8" });
  const archiveRow = popup.locator("#resources-list li", { hasText: "download.zip" });

  await expect(manifestRow).toBeVisible();
  await expect(manifestRow.locator(".resource-kind")).toHaveText("HLS manifest");
  await expect(manifestRow).not.toContainText("Size unknown");
  await expect(archiveRow).toBeVisible();
  await expect(archiveRow.locator(".resource-kind")).toHaveText("Archive");
  await expect(archiveRow).not.toContainText("Size unknown");

  const list = popup.locator("#resources-list");
  await expect(list).not.toContainText("segment1.ts");
  await expect(list).not.toContainText("segment2.ts");
});

test("sends exactly one credential-free prefill capture for the archive", async () => {
  const tabId = requireTabId();
  await waitForResource(tabId, "/download.zip");

  const popup = await reloadPopup();
  const archiveRow = popup.locator("#resources-list li", { hasText: "download.zip" });
  const send = archiveRow.locator("button.resource-send");
  await expect(send).toBeEnabled();
  await send.click();

  await expect(popup.locator("#notice")).toHaveText("Sent to OneGet.");
  await expect.poll(() => captures.length, { timeout: 10_000 }).toBe(1);

  const capture = captures[0];
  expect(capture.body.intent).toBe("prefill");
  expect(capture.body.items).toHaveLength(1);
  const item = capture.body.items?.[0];
  expect(item?.url).toMatch(/\/download\.zip$/);
  expect(item?.kind).toBe("file");
  expect(item?.headers).toEqual([{ name: "Referer", value: `${FIXTURE_ORIGIN}/` }]);

  expect(capture.headers.authorization).toBe(`Bearer ${PAIRING_TOKEN}`);
  expect(capture.headers.origin).toBe(extensionOrigin());
  expect(capture.headers.cookie).toBeUndefined();
});

test("clears the previous page resources after an SPA route change", async () => {
  const fixture = requirePage(fixturePage, "fixture");
  const tabId = requireTabId();
  await waitForResource(tabId, "/download.zip");

  await fixture.click("#spa-route");
  await expect(fixture.locator("#route-label")).toHaveText("route: /spa/route-2");
  await expect
    .poll(async () => (await listResourceUrls(tabId)).some((url) => url.endsWith("/download.zip")), {
      timeout: 10_000,
    })
    .toBe(false);

  await fixture.click("#fetch-second");
  await waitForResource(tabId, "/spa/second-resource.zip");

  const popup = await reloadPopup();
  await expect(popup.locator("#resources-list li", { hasText: "second-resource.zip" })).toBeVisible();
  const list = popup.locator("#resources-list");
  await expect(list).not.toContainText("download.zip");
  await expect(list).not.toContainText("master.m3u8");
  await expect(list).not.toContainText("segment1.ts");
  await expect(list).not.toContainText("segment2.ts");
});

test("shows one localized video overlay button that creates no capture", async () => {
  const fixture = requirePage(fixturePage, "fixture");
  await fixture.hover("#fixture-video");

  const button = fixture.locator(OVERLAY_BUTTON);
  await expect(button).toHaveCount(1);
  await expect(button).toBeVisible();
  await expect(button).toHaveAccessibleName("Download with OneGet");

  await button.click();
  const notice = fixture.locator(OVERLAY_NOTICE);
  await expect(notice).toBeVisible();
  await expect(notice).toHaveText("Video download is not available yet.");
  await expect.poll(() => captures.length, { timeout: 5_000 }).toBe(0);
});

test("takes over a matched browser download when onDeterminingFilename is available", async () => {
  const serviceWorker = requireWorker();
  let supported: boolean;
  try {
    supported = await serviceWorker.evaluate(() => {
      const chromeApi = (globalThis as unknown as { chrome: ChromeRuntime }).chrome;
      return typeof chromeApi.downloads?.onDeterminingFilename?.addListener === "function";
    });
  } catch {
    supported = false;
  }

  test.skip(!supported, "onDeterminingFilename unavailable in this headless Chromium");
  if (!supported) {
    return;
  }

  const popup = await reloadPopup();
  await popup.locator("#takeover-enabled").check();
  await popup.locator("#minimum-size").fill("0");
  await expect(popup.locator("#category-other")).toBeChecked();
  await popup.locator("#settings-form button[type=submit]").click();
  await expect(popup.locator("#notice")).toHaveText("Settings saved.");

  const fixture = requirePage(fixturePage, "fixture");
  await fixture.evaluate(() => {
    const anchor = document.createElement("a");
    anchor.id = "takeover-link";
    anchor.href = "/takeover.bin";
    anchor.download = "takeover.bin";
    anchor.textContent = "takeover";
    document.body.append(anchor);
  });
  await fixture.click("#takeover-link");

  await expect
    .poll(() => captures.filter((capture) => capture.body.intent === "takeover").length, {
      timeout: 15_000,
    })
    .toBe(1);

  const takeover = captures.find((capture) => capture.body.intent === "takeover");
  expect(takeover).toBeDefined();
  expect(takeover?.body.items).toHaveLength(1);
  expect(takeover?.body.items?.[0]?.url).toMatch(/\/takeover\.bin$/);
  expect(takeover?.headers.authorization).toBe(`Bearer ${PAIRING_TOKEN}`);
  expect(takeover?.headers.origin).toBe(extensionOrigin());

  await expect
    .poll(
      async () =>
        serviceWorker.evaluate(async () => {
          const chromeApi = (globalThis as unknown as { chrome: ChromeRuntime }).chrome;
          const items = (await chromeApi.downloads?.search({})) ?? [];
          return items.filter(
            (item) => typeof item.url === "string" && item.url.endsWith("/takeover.bin"),
          ).length;
        }),
      { timeout: 10_000 },
    )
    .toBe(0);

  await expect
    .poll(() => captures.filter((capture) => capture.body.intent === "takeover").length)
    .toBe(1);
});

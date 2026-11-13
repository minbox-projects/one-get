import { createBackgroundController } from "../shared/background";
import type {
  BackgroundController,
  BrowserDownload,
  RuntimeMessage,
} from "../shared/background";
import { createBrowserAdapter } from "../shared/browser";
import type { BrowserApi } from "../shared/browser";
import { filenameFromUrl, sanitizeFilename } from "../shared/filename";

declare const __ONEGET_API_ORIGIN__: string;

const DEFAULT_API_ORIGIN = "http://127.0.0.1:37652";
const PAIR_LABEL = "Firefox extension";

export function resolveApiOrigin(): string {
  return typeof __ONEGET_API_ORIGIN__ === "string" ? __ONEGET_API_ORIGIN__ : DEFAULT_API_ORIGIN;
}

function parseContentLength(value: string | null): number | null {
  if (value === null) {
    return null;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function downloadFilename(item: FirefoxDownloadItem): string {
  const segments = (item.filename ?? "").split(/[\\/]+/);
  const basename = sanitizeFilename(segments[segments.length - 1] ?? "", "");
  return basename.length > 0 ? basename : filenameFromUrl(item.url);
}

export function mapFirefoxDownload(item: FirefoxDownloadItem): BrowserDownload {
  return {
    id: item.id,
    url: item.url,
    filename: downloadFilename(item),
    referer: item.referrer,
    mime: item.mime,
    totalBytes: typeof item.totalBytes === "number" ? item.totalBytes : null,
    incognito: item.incognito === true,
    byExtension: typeof item.byExtensionId === "string" && item.byExtensionId.length > 0,
  };
}

export interface FirefoxBackgroundDependencies {
  browser: FirefoxBrowser;
  fetchImpl?: typeof fetch;
  apiOrigin: string;
  now: () => number;
}

export function createFirefoxBackground(
  dependencies: FirefoxBackgroundDependencies,
): BackgroundController {
  const api = dependencies.browser;
  const adapter = createBrowserAdapter(api as unknown as BrowserApi, { messaging: "promise" });

  const controller = createBackgroundController({
    storage: adapter.storage("local"),
    session: adapter.storage("session"),
    fetchImpl: dependencies.fetchImpl ?? globalThis.fetch.bind(globalThis),
    apiOrigin: dependencies.apiOrigin,
    now: dependencies.now,
    pairLabel: PAIR_LABEL,
    persistTakeoverFailure: true,
    downloads: {
      async cancel(id) {
        await api.downloads.cancel(id);
      },
      async erase(query) {
        await api.downloads.erase(query);
      },
    },
    log: (event, fields) => {
      console.debug("[oneget]", event, fields ?? {});
    },
  });

  adapter.onMessage((message, sender) =>
    controller
      .handleRuntimeMessage(message as RuntimeMessage, {
        tabId: sender.tab?.id,
        frameId: sender.frameId,
        pageUrl: sender.tab?.url,
      })
      .catch(() => ({
        ok: false,
        error: { code: "INTERNAL", message: "internal error" },
      })),
  );

  api.webRequest.onResponseStarted.addListener(
    (details) => {
      if (details.tabId < 0) {
        return;
      }
      void controller.observeResource({
        url: details.url,
        tabId: details.tabId,
        frameId: details.frameId,
        pageUrl: "",
        nowMs: dependencies.now(),
        mime: adapter.responseHeader(details.responseHeaders, "content-type"),
        size: parseContentLength(
          adapter.responseHeader(details.responseHeaders, "content-length"),
        ),
      });
    },
    { urls: ["http://*/*", "https://*/*"] },
    ["responseHeaders"],
  );

  api.downloads.onCreated.addListener((item) => {
    void controller.handleDownload(mapFirefoxDownload(item));
  });

  api.webNavigation.onCommitted.addListener((details) => {
    void controller.handleNavigation(details.tabId, details.frameId, details.url);
  });

  api.webNavigation.onHistoryStateUpdated.addListener((details) => {
    void controller.handleNavigation(details.tabId, details.frameId, details.url);
  });

  api.tabs.onRemoved.addListener((tabId) => {
    void controller.handleTabRemoved(tabId);
  });

  return controller;
}

export function installFirefoxBackground(): void {
  const api = typeof browser === "undefined" ? undefined : browser;
  if (api === undefined || api.runtime.onMessage === undefined) {
    return;
  }
  createFirefoxBackground({
    browser: api,
    apiOrigin: resolveApiOrigin(),
    now: () => Date.now(),
  });
}

if (typeof browser !== "undefined" && browser.runtime.onMessage !== undefined) {
  installFirefoxBackground();
}

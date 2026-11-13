import { createBackgroundController } from "../shared/background";
import type { RuntimeMessage } from "../shared/background";
import { createBrowserAdapter } from "../shared/browser";
import type { BrowserApi } from "../shared/browser";

declare const __ONEGET_API_ORIGIN__: string;

const DEFAULT_API_ORIGIN = "http://127.0.0.1:37652";

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

export function installChromeBackground(): void {
  if (typeof chrome === "undefined" || chrome.runtime?.onMessage === undefined) {
    return;
  }

  const adapter = createBrowserAdapter(chrome as unknown as BrowserApi, {
    messaging: "callback",
  });
  const controller = createBackgroundController({
    storage: adapter.storage("local"),
    session: adapter.storage("session"),
    fetchImpl: globalThis.fetch.bind(globalThis),
    apiOrigin: resolveApiOrigin(),
    now: () => Date.now(),
    downloads: {
      async cancel(id) {
        await chrome.downloads.cancel(id);
      },
      async erase(query) {
        await chrome.downloads.erase(query);
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

  chrome.webRequest.onResponseStarted.addListener(
    (details) => {
      if (details.tabId < 0) {
        return;
      }
      void controller.observeResource({
        url: details.url,
        tabId: details.tabId,
        frameId: details.frameId,
        pageUrl: "",
        nowMs: Date.now(),
        mime: adapter.responseHeader(details.responseHeaders, "content-type"),
        size: parseContentLength(
          adapter.responseHeader(details.responseHeaders, "content-length"),
        ),
      });
    },
    { urls: ["http://*/*", "https://*/*"] },
    ["responseHeaders"],
  );

  chrome.downloads.onDeterminingFilename.addListener((item, suggest) => {
    const finish = () => {
      try {
        suggest();
      } catch {
        // The download may already be canceled or erased after a takeover.
      }
    };
    void controller
      .handleDownload({
        id: item.id,
        url: item.url,
        filename: item.filename,
        referer: item.referrer,
        mime: item.mime,
        totalBytes: item.totalBytes,
        incognito: item.incognito,
        byExtension: typeof item.byExtensionId === "string" && item.byExtensionId.length > 0,
      })
      .then(finish, finish);
    return true;
  });

  chrome.webNavigation.onCommitted.addListener((details) => {
    void controller.handleNavigation(details.tabId, details.frameId, details.url);
  });

  chrome.tabs.onRemoved.addListener((tabId) => {
    void controller.handleTabRemoved(tabId);
  });
}

if (typeof chrome !== "undefined" && chrome.runtime?.onMessage !== undefined) {
  installChromeBackground();
}

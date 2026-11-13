// Minimal promise-style WebExtension surface used by the Firefox background entry.
// Firefox exposes these APIs on the global `browser` object, unlike Chromium's
// callback-style `chrome` namespace.

interface FirefoxResponseHeader {
  name?: string;
  value?: string;
}

interface FirefoxDownloadItem {
  id: number;
  url: string;
  filename: string;
  referrer?: string;
  mime?: string;
  totalBytes?: number;
  incognito?: boolean;
  byExtensionId?: string;
}

interface FirefoxDownloadQuery {
  id: number;
}

interface FirefoxRequestDetails {
  requestId?: string;
  tabId: number;
  frameId: number;
  url: string;
  responseHeaders?: FirefoxResponseHeader[];
}

interface FirefoxNavigationDetails {
  tabId: number;
  frameId: number;
  url: string;
}

interface FirefoxMessageSender {
  tab?: { id?: number; url?: string };
  frameId?: number;
}

interface FirefoxStorageArea {
  get(keys: string | string[] | null): Promise<Record<string, unknown>>;
  set(items: Record<string, unknown>): Promise<void>;
  remove(keys: string | string[]): Promise<void>;
}

interface FirefoxEvent<TListener> {
  addListener(listener: TListener): void;
}

interface FirefoxWebRequestEvent<TListener> {
  addListener(
    listener: TListener,
    filter?: { urls?: string[] },
    extraInfoSpec?: readonly string[],
  ): void;
}

interface FirefoxBrowser {
  runtime: {
    onMessage?: FirefoxEvent<
      (message: unknown, sender: FirefoxMessageSender) => unknown
    >;
  };
  storage: {
    local: FirefoxStorageArea;
    session: FirefoxStorageArea;
  };
  downloads: {
    onCreated: FirefoxEvent<(item: FirefoxDownloadItem) => void>;
    cancel(id: number): Promise<void>;
    erase(query: FirefoxDownloadQuery): Promise<void>;
  };
  webRequest: {
    onResponseStarted: FirefoxWebRequestEvent<(details: FirefoxRequestDetails) => void>;
  };
  webNavigation: {
    onCommitted: FirefoxEvent<(details: FirefoxNavigationDetails) => void>;
    onHistoryStateUpdated: FirefoxEvent<(details: FirefoxNavigationDetails) => void>;
  };
  tabs: {
    onRemoved: FirefoxEvent<(tabId: number) => void>;
  };
}

declare const browser: FirefoxBrowser | undefined;

const MARKER = "oneget-inject";

interface ResourcePayload {
  url: string;
  mime: string | null;
  size: number | null;
}

function isHttpUrl(value: string): boolean {
  return value.startsWith("http://") || value.startsWith("https://");
}

function postMessageToExtension(kind: string, payload: unknown): void {
  window.postMessage({ source: MARKER, kind, payload }, "*");
}

function parseSize(value: string | null): number | null {
  if (value === null) {
    return null;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function observe(url: string, mime: string | null, size: number | null): void {
  if (!isHttpUrl(url)) {
    return;
  }
  const payload: ResourcePayload = { url, mime, size };
  postMessageToExtension("resource", payload);
}

const originalFetch = window.fetch.bind(window);

window.fetch = async function observedFetch(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const response = await originalFetch(input, init);
  try {
    const requested =
      typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    observe(
      response.url.length > 0 ? response.url : requested,
      response.headers.get("content-type"),
      parseSize(response.headers.get("content-length")),
    );
  } catch {
    // Observation must never disturb the page's own request.
  }
  return response;
};

const xhrUrls = new WeakMap<XMLHttpRequest, string>();
type OpenFunction = typeof XMLHttpRequest.prototype.open;
type SendFunction = typeof XMLHttpRequest.prototype.send;

const originalOpen: OpenFunction = XMLHttpRequest.prototype.open;
XMLHttpRequest.prototype.open = function observedOpen(
  this: XMLHttpRequest,
  ...args: Parameters<OpenFunction>
) {
  const target = args[1];
  xhrUrls.set(this, typeof target === "string" ? target : target.href);
  return originalOpen.apply(this, args);
} as OpenFunction;

const originalSend: SendFunction = XMLHttpRequest.prototype.send;
XMLHttpRequest.prototype.send = function observedSend(
  this: XMLHttpRequest,
  ...args: Parameters<SendFunction>
) {
  this.addEventListener(
    "loadend",
    () => {
      const url = this.responseURL.length > 0 ? this.responseURL : (xhrUrls.get(this) ?? "");
      observe(
        url,
        this.getResponseHeader("content-type"),
        parseSize(this.getResponseHeader("content-length")),
      );
    },
    { once: true },
  );
  return originalSend.apply(this, args);
} as SendFunction;

function reportNavigation(): void {
  postMessageToExtension("spa", { pageUrl: window.location.href });
}

const originalPushState = window.history.pushState.bind(window.history);
window.history.pushState = function observedPushState(
  ...args: Parameters<History["pushState"]>
): void {
  originalPushState(...args);
  reportNavigation();
};

const originalReplaceState = window.history.replaceState.bind(window.history);
window.history.replaceState = function observedReplaceState(
  ...args: Parameters<History["replaceState"]>
): void {
  originalReplaceState(...args);
  reportNavigation();
};

window.addEventListener("popstate", reportNavigation);
window.addEventListener("hashchange", reportNavigation);

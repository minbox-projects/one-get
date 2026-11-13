import { classifyResource } from "../shared/classify";
import { decideTakeover } from "../shared/filter";
import type { TakeoverCandidate } from "../shared/filter";
import { ResourceStore } from "../shared/pageStore";
import { API_PROTOCOL_VERSION, requestJson } from "../shared/protocol";
import type { ApiErrorBody, ApiResult } from "../shared/protocol";
import { normalizeSettings } from "../shared/settings";
import type { ExtensionSettings } from "../shared/settings";
import {
  clearToken,
  isValidToken,
  loadSettingsDocument,
  loadToken,
  saveSettingsDocument,
  saveToken,
} from "../shared/storage";
import type { StorageArea } from "../shared/storage";
import type { CaptureDraftItem, ResourceEntry, ResourceInput } from "../shared/types";

declare const __ONEGET_API_ORIGIN__: string;

const DEFAULT_API_ORIGIN = "http://127.0.0.1:37652";
const RESOURCES_KEY = "oneget.resources";
const SESSION_RESOURCE_BOUND = 5000;
const REVOKE_TIMEOUT_MS = 3000;

export interface BrowserDownload {
  id: number;
  url: string;
  filename: string;
  referer?: string;
  mime?: string;
  totalBytes?: number | null;
  incognito?: boolean;
  byExtension?: boolean;
}

export type DownloadAllowReason =
  | "unpaired"
  | "disabled"
  | "non_http"
  | "incognito"
  | "extension_download"
  | "blacklisted"
  | "manifest"
  | "unsupported_kind"
  | "category_disabled"
  | "too_small"
  | "duplicate"
  | "capture_failed"
  | "token_revoked"
  | "update_required";

export type DownloadOutcome =
  | { action: "takeover"; taskId: string; reason?: undefined }
  | { action: "allow"; reason: DownloadAllowReason; taskId?: string };

export interface PairOutcome {
  ok: boolean;
  status?: number;
  error?: ApiErrorBody;
  label?: string;
}

export type TakeoverFailureReason =
  | "capture_failed"
  | "token_revoked"
  | "update_required";

export interface TakeoverFailure {
  downloadId: number;
  url: string;
  filename: string;
  reason: TakeoverFailureReason;
}

export interface RuntimeMessage {
  type?: unknown;
  [key: string]: unknown;
}

export interface RuntimeSender {
  tabId?: number;
  frameId?: number;
  pageUrl?: string;
}

export interface DownloadControl {
  cancel(id: number): Promise<void>;
  erase(query: { id: number }): Promise<void>;
}

export interface BackgroundDependencies {
  storage: StorageArea;
  session: StorageArea;
  fetchImpl: typeof fetch;
  apiOrigin: string;
  now: () => number;
  downloads: DownloadControl;
  log?: (event: string, fields?: Record<string, unknown>) => void;
}

export interface BackgroundController {
  handleDownload(download: BrowserDownload): Promise<DownloadOutcome>;
  handleRuntimeMessage(message: RuntimeMessage, sender?: RuntimeSender): Promise<unknown>;
  handleNavigation(tabId: number, frameId: number, pageUrl: string): Promise<void>;
  handleTabRemoved(tabId: number): Promise<void>;
  observeResource(input: ResourceInput): Promise<ResourceEntry | null>;
  listResources(tabId: number): Promise<ResourceEntry[]>;
  getSettings(): Promise<ExtensionSettings>;
  saveSettings(settings: ExtensionSettings): Promise<ExtensionSettings>;
  pair(code: string): Promise<PairOutcome>;
  unpair(): Promise<void>;
}

interface CaptureResponse {
  accepted?: number;
  duplicate?: boolean;
  taskId?: string;
  rejected?: Array<{ code?: string }>;
}

interface PairResponse {
  token?: string;
  client?: { id?: string; label?: string };
}

interface StoredResource {
  url?: unknown;
  tabId?: unknown;
  frameId?: unknown;
  pageUrl?: unknown;
  capturedAtMs?: unknown;
  filename?: unknown;
  mime?: unknown;
  size?: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function numberOr(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function stringOr(value: unknown, fallback: string): string {
  return typeof value === "string" ? value : fallback;
}

function nullableString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function nullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function sizeOfDownload(value: number | null | undefined): number | null {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : null;
}

function headerValue(
  headers: chrome.webRequest.HttpHeader[] | undefined,
  name: string,
): string | null {
  if (headers === undefined) {
    return null;
  }
  const wanted = name.toLowerCase();
  for (const header of headers) {
    if (typeof header.name === "string" && header.name.toLowerCase() === wanted) {
      return typeof header.value === "string" ? header.value : null;
    }
  }
  return null;
}

function parseContentLength(value: string | null): number | null {
  if (value === null) {
    return null;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function serializeResources(entries: ResourceEntry[]): StoredResource[] {
  return entries.slice(0, SESSION_RESOURCE_BOUND).map((entry) => ({
    url: entry.url,
    tabId: entry.tabId,
    frameId: entry.frameId,
    pageUrl: entry.pageUrl,
    capturedAtMs: entry.capturedAtMs,
    filename: entry.filename,
    mime: entry.mime,
    size: entry.size,
  }));
}

function deserializeResources(value: unknown): StoredResource[] {
  return Array.isArray(value) ? value.filter(isRecord) : [];
}

function createResourceStoreFromSnapshot(value: unknown): ResourceStore {
  const store = new ResourceStore();
  for (const raw of deserializeResources(value)) {
    const url = stringOr(raw.url, "");
    if (url.length === 0) {
      continue;
    }
    store.add({
      url,
      tabId: numberOr(raw.tabId, -1),
      frameId: numberOr(raw.frameId, 0),
      pageUrl: stringOr(raw.pageUrl, ""),
      nowMs: numberOr(raw.capturedAtMs, 0),
      filename: typeof raw.filename === "string" ? raw.filename : undefined,
      mime: nullableString(raw.mime),
      size: nullableNumber(raw.size),
    });
  }
  return store;
}

export function resolveApiOrigin(): string {
  return typeof __ONEGET_API_ORIGIN__ === "string" ? __ONEGET_API_ORIGIN__ : DEFAULT_API_ORIGIN;
}

export function createBackgroundController(
  dependencies: BackgroundDependencies,
): BackgroundController {
  const log = dependencies.log ?? (() => {});
  const processed = new Map<string, string>();
  const trackedTabs = new Set<number>();
  let storePromise: Promise<ResourceStore> | null = null;
  let prefillSequence = 0;
  let takeoverError: TakeoverFailure | null = null;

  function storeKey(token: string, downloadId: number): string {
    return `${token}:${downloadId}`;
  }

  function rememberTakeoverFailure(
    download: BrowserDownload,
    reason: TakeoverFailureReason,
  ): void {
    takeoverError = {
      downloadId: download.id,
      url: download.url,
      filename: download.filename,
      reason,
    };
  }

  async function loadStore(): Promise<ResourceStore> {
    if (storePromise !== null) {
      return storePromise;
    }
    storePromise = (async () => {
      let snapshot: unknown;
      try {
        const values = await dependencies.session.get(RESOURCES_KEY);
        snapshot = values[RESOURCES_KEY];
      } catch {
        snapshot = undefined;
      }
      const store = createResourceStoreFromSnapshot(snapshot);
      for (const raw of deserializeResources(snapshot)) {
        if (typeof raw.tabId === "number") {
          trackedTabs.add(raw.tabId);
        }
      }
      return store;
    })();
    return storePromise;
  }

  async function persistStore(store: ResourceStore): Promise<void> {
    const entries: ResourceEntry[] = [];
    for (const tabId of trackedTabs) {
      entries.push(...store.list(tabId));
    }
    try {
      await dependencies.session.set({ [RESOURCES_KEY]: serializeResources(entries) });
    } catch {
      log("resources.persist_failed");
    }
  }

  async function currentSettings(): Promise<ExtensionSettings> {
    const document = await loadSettingsDocument(dependencies.storage);
    return normalizeSettings(document);
  }

  async function observeResource(input: ResourceInput): Promise<ResourceEntry | null> {
    const store = await loadStore();
    const entry = store.add(input);
    if (entry !== null) {
      trackedTabs.add(input.tabId);
      await persistStore(store);
    }
    return entry;
  }

  async function listResources(tabId: number): Promise<ResourceEntry[]> {
    const store = await loadStore();
    return store.list(tabId);
  }

  function safeHeaders(referer: string | undefined): Array<{ name: string; value: string }> {
    if (typeof referer !== "string" || referer.length === 0) {
      return [];
    }
    return [{ name: "Referer", value: referer }];
  }

  async function sendCapture(
    token: string,
    intent: "takeover" | "prefill",
    sourceId: string,
    items: CaptureDraftItem[],
  ): Promise<ApiResult<CaptureResponse>> {
    return requestJson<CaptureResponse>({
      origin: dependencies.apiOrigin,
      path: "/capture",
      method: "POST",
      token,
      body: {
        protocolVersion: API_PROTOCOL_VERSION,
        intent,
        sourceId,
        items,
      },
      fetchImpl: dependencies.fetchImpl,
    });
  }

  async function handleDownload(download: BrowserDownload): Promise<DownloadOutcome> {
    const token = await loadToken(dependencies.storage);
    if (token === null) {
      return { action: "allow", reason: "unpaired" };
    }

    const settings = await currentSettings();
    const classification = classifyResource({
      url: download.url,
      mime: download.mime,
      filename: download.filename,
    });
    const candidate: TakeoverCandidate = {
      url: download.url,
      kind: classification.kind,
      category: classification.category,
      size: sizeOfDownload(download.totalBytes),
      incognito: download.incognito === true,
      fromExtension: download.byExtension === true,
    };
    const decision = decideTakeover(candidate, {
      takeoverEnabled: settings.takeoverEnabled,
      categories: settings.categories,
      minimumSizeBytes: settings.minimumSizeBytes,
      blacklistHosts: settings.blacklistHosts,
    });
    if (!decision.takeOver) {
      return { action: "allow", reason: decision.reason };
    }

    const key = storeKey(token, download.id);
    const existingTask = processed.get(key);
    if (existingTask !== undefined) {
      return { action: "allow", reason: "duplicate", taskId: existingTask };
    }

    const item: CaptureDraftItem = {
      url: download.url,
      filename: download.filename,
      referer: download.referer,
      mime: download.mime,
      size: sizeOfDownload(download.totalBytes),
      kind: "file",
      headers: safeHeaders(download.referer),
    };
    const result = await sendCapture(token, "takeover", `takeover:${download.id}`, [item]);

    if (!result.ok) {
      if (result.status === 401) {
        await clearToken(dependencies.storage);
        rememberTakeoverFailure(download, "token_revoked");
        log("capture.token_revoked", { intent: "takeover" });
        return { action: "allow", reason: "token_revoked" };
      }
      if (result.status === 409) {
        rememberTakeoverFailure(download, "update_required");
        log("capture.update_required", { intent: "takeover" });
        return { action: "allow", reason: "update_required" };
      }
      rememberTakeoverFailure(download, "capture_failed");
      log("capture.failed", { intent: "takeover", status: result.status, code: result.error.code });
      return { action: "allow", reason: "capture_failed" };
    }

    const taskId = result.data.taskId;
    if (typeof taskId !== "string" || taskId.length === 0) {
      rememberTakeoverFailure(download, "capture_failed");
      log("capture.failed", { intent: "takeover", status: result.status, code: "NO_TASK" });
      return { action: "allow", reason: "capture_failed" };
    }

    takeoverError = null;

    if (result.data.duplicate === true) {
      processed.set(key, taskId);
      log("capture.duplicate", { intent: "takeover", taskId });
      return { action: "allow", reason: "duplicate", taskId };
    }

    processed.set(key, taskId);
    try {
      await dependencies.downloads.cancel(download.id);
      await dependencies.downloads.erase({ id: download.id });
    } catch {
      log("download.cleanup_failed", { downloadId: download.id });
    }
    log("capture.succeeded", { intent: "takeover", taskId });
    return { action: "takeover", taskId };
  }

  async function pair(code: string): Promise<PairOutcome> {
    const result = await requestJson<PairResponse>({
      origin: dependencies.apiOrigin,
      path: "/pair",
      method: "POST",
      body: { protocolVersion: API_PROTOCOL_VERSION, code },
      fetchImpl: dependencies.fetchImpl,
    });

    if (!result.ok) {
      log("pair.failed", { status: result.status, code: result.error.code });
      return { ok: false, status: result.status, error: result.error };
    }

    const token = result.data.token;
    if (!isValidToken(token)) {
      log("pair.failed", { status: result.status, code: "INVALID_TOKEN" });
      return {
        ok: false,
        status: result.status,
        error: { code: "INVALID_TOKEN", message: "invalid pairing response" },
      };
    }

    await saveToken(dependencies.storage, token);
    const label = result.data.client?.label;
    log("pair.succeeded", { clientId: result.data.client?.id });
    return { ok: true, status: result.status, label };
  }

  async function unpair(): Promise<void> {
    const token = await loadToken(dependencies.storage);
    if (token !== null) {
      await requestJson({
        origin: dependencies.apiOrigin,
        path: "/pair/revoke",
        method: "POST",
        token,
        body: { protocolVersion: API_PROTOCOL_VERSION },
        fetchImpl: dependencies.fetchImpl,
        timeoutMs: REVOKE_TIMEOUT_MS,
      });
    }
    await clearToken(dependencies.storage);
    processed.clear();
    log("pair.revoked");
  }

  async function getSettings(): Promise<ExtensionSettings> {
    return currentSettings();
  }

  async function saveSettings(settings: ExtensionSettings): Promise<ExtensionSettings> {
    const existing = await loadSettingsDocument(dependencies.storage);
    const normalized = normalizeSettings(settings);
    await saveSettingsDocument(dependencies.storage, existing, normalized);
    log("settings.saved", { takeoverEnabled: normalized.takeoverEnabled });
    return normalized;
  }

  async function handleNavigation(
    tabId: number,
    frameId: number,
    pageUrl: string,
  ): Promise<void> {
    const store = await loadStore();
    if (frameId === 0) {
      store.navigateTab(tabId, pageUrl);
    } else {
      store.navigateFrame(tabId, frameId, pageUrl);
    }
    trackedTabs.add(tabId);
    await persistStore(store);
  }

  async function handleTabRemoved(tabId: number): Promise<void> {
    const store = await loadStore();
    store.clearTab(tabId);
    trackedTabs.delete(tabId);
    await persistStore(store);
  }

  async function pairingState(): Promise<{ paired: boolean; label?: string }> {
    const token = await loadToken(dependencies.storage);
    return { paired: token !== null };
  }

  async function capturePrefill(message: RuntimeMessage, sender?: RuntimeSender) {
    const token = await loadToken(dependencies.storage);
    if (token === null) {
      return { ok: false, error: { code: "UNPAIRED", message: "pair OneGet first" } };
    }

    const resource = isRecord(message.resource)
      ? message.resource
      : isRecord(message.item)
        ? message.item
        : undefined;
    const url = stringOr(
      message.resourceUrl ?? message.url ?? resource?.url,
      "",
    );
    if (url.length === 0) {
      return { ok: false, error: { code: "INVALID_ITEM", message: "missing url" } };
    }

    const tabId = numberOr(message.tabId ?? sender?.tabId, -1);
    const pageUrl = stringOr(resource?.pageUrl ?? sender?.pageUrl, "");
    const item: CaptureDraftItem = {
      url,
      filename: stringOr(resource?.filename, ""),
      referer: pageUrl.length > 0 ? pageUrl : undefined,
      mime: nullableString(resource?.mime) ?? undefined,
      size: nullableNumber(resource?.size),
      kind: "file",
      headers: safeHeaders(pageUrl.length > 0 ? pageUrl : undefined),
    };
    prefillSequence += 1;
    const sourceId = `prefill:${tabId}:${dependencies.now()}:${prefillSequence}`;
    const result = await sendCapture(token, "prefill", sourceId, [item]);

    if (!result.ok) {
      if (result.status === 401) {
        await clearToken(dependencies.storage);
        log("capture.token_revoked", { intent: "prefill" });
        return { ok: false, error: result.error, reason: "token_revoked" };
      }
      if (result.status === 409) {
        log("capture.update_required", { intent: "prefill" });
        return { ok: false, error: result.error, reason: "update_required" };
      }
      log("capture.failed", { intent: "prefill", status: result.status, code: result.error.code });
      return { ok: false, error: result.error, reason: "capture_failed" };
    }

    if (takeoverError !== null && takeoverError.url === url) {
      takeoverError = null;
    }
    log("capture.succeeded", { intent: "prefill" });
    return { ok: true, duplicate: result.data.duplicate === true, taskId: result.data.taskId };
  }

  async function handleRuntimeMessage(
    message: RuntimeMessage,
    sender?: RuntimeSender,
  ): Promise<unknown> {
    const type = typeof message.type === "string" ? message.type : "";

    switch (type) {
      case "popup/get-state": {
        const tabId = numberOr(message.tabId ?? sender?.tabId, -1);
        const [resources, settings, pairing] = await Promise.all([
          listResources(tabId),
          getSettings(),
          pairingState(),
        ]);
        return {
          resources,
          settings,
          paired: pairing.paired,
          pairing,
          takeoverError,
        };
      }
      case "popup/capture-prefill":
      case "capture:prefill": {
        return capturePrefill(message, sender);
      }
      case "popup/pair":
      case "pairing:pair": {
        const code = stringOr(message.code, "");
        return pair(code);
      }
      case "popup/unpair":
      case "pairing:unpair": {
        await unpair();
        return null;
      }
      case "settings:get": {
        return getSettings();
      }
      case "settings/save":
      case "settings:save":
      case "settings/changed": {
        const settings = message.settings ?? message.value;
        if (!isRecord(settings)) {
          return { ok: false, error: { code: "INVALID_SETTINGS", message: "invalid settings" } };
        }
        return saveSettings(normalizeSettings(settings));
      }
      case "pairing/status":
      case "pairing:status": {
        return pairingState();
      }
      case "resources:list":
      case "resources/list": {
        const tabId = numberOr(message.tabId ?? sender?.tabId, -1);
        return listResources(tabId);
      }
      case "resources:clear":
      case "resources/clear": {
        const tabId = numberOr(message.tabId ?? sender?.tabId, -1);
        const frameId = numberOr(message.frameId ?? sender?.frameId, 0);
        const store = await loadStore();
        store.clearFrame(tabId, frameId);
        await persistStore(store);
        return null;
      }
      case "content/resource": {
        const tabId = numberOr(sender?.tabId ?? message.tabId, -1);
        if (tabId < 0) {
          return null;
        }
        return observeResource({
          url: stringOr(message.url, ""),
          tabId,
          frameId: numberOr(sender?.frameId ?? message.frameId, 0),
          pageUrl: stringOr(message.pageUrl ?? sender?.pageUrl, ""),
          nowMs: dependencies.now(),
          filename: typeof message.filename === "string" ? message.filename : undefined,
          mime: nullableString(message.mime),
          size: nullableNumber(message.size),
        });
      }
      case "content/spa": {
        const tabId = numberOr(sender?.tabId ?? message.tabId, -1);
        if (tabId < 0) {
          return null;
        }
        const frameId = numberOr(sender?.frameId ?? message.frameId, 0);
        await handleNavigation(tabId, frameId, stringOr(message.pageUrl, ""));
        return null;
      }
      case "content/clear-frame": {
        const tabId = numberOr(sender?.tabId ?? message.tabId, -1);
        if (tabId < 0) {
          return null;
        }
        const store = await loadStore();
        store.clearFrame(tabId, numberOr(sender?.frameId, 0));
        await persistStore(store);
        return null;
      }
      default:
        return { ok: false, error: { code: "UNKNOWN_MESSAGE", message: "unknown message" } };
    }
  }

  return {
    handleDownload,
    handleRuntimeMessage,
    handleNavigation,
    handleTabRemoved,
    observeResource,
    listResources,
    getSettings,
    saveSettings,
    pair,
    unpair,
  };
}

function chromeStorageArea(area: chrome.storage.StorageArea): StorageArea {
  return {
    async get(keys) {
      const values = await area.get(keys === null ? null : keys);
      return values as Record<string, unknown>;
    },
    async set(items) {
      await area.set(items);
    },
    async remove(keys) {
      await area.remove(keys);
    },
  };
}

export function installChromeBackground(): void {
  if (typeof chrome === "undefined" || chrome.runtime?.onMessage === undefined) {
    return;
  }

  const controller = createBackgroundController({
    storage: chromeStorageArea(chrome.storage.local),
    session: chromeStorageArea(chrome.storage.session),
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

  chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
    void controller
      .handleRuntimeMessage(message as RuntimeMessage, {
        tabId: sender.tab?.id,
        frameId: sender.frameId,
        pageUrl: sender.tab?.url,
      })
      .then((value) => {
        sendResponse(value);
      })
      .catch(() => {
        sendResponse({ ok: false, error: { code: "INTERNAL", message: "internal error" } });
      });
    return true;
  });

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
        mime: headerValue(details.responseHeaders, "content-type"),
        size: parseContentLength(headerValue(details.responseHeaders, "content-length")),
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

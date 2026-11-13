import type { TakeoverFailure } from "../shared/background";
import { redactUrl } from "../shared/filename";
import { resolveExtensionLanguage, translateExtension } from "../shared/i18n";
import type { ExtensionLanguage } from "../shared/i18n";
import { DEFAULT_SETTINGS, normalizeSettings } from "../shared/settings";
import type { ExtensionSettings } from "../shared/settings";
import type { ResourceCategory, ResourceEntry, ResourceKind } from "../shared/types";
import { RESOURCE_CATEGORIES } from "../shared/types";

const NOTICE_TIMEOUT_MS = 3500;
const CATEGORY_LABEL_KEYS: Record<ResourceCategory, string> = {
  video: "popup.kind.video",
  audio: "popup.kind.audio",
  archive: "popup.kind.archive",
  program: "popup.kind.program",
  document: "popup.kind.document",
  model: "popup.kind.model",
  other: "popup.kind.other",
};

export interface ResourceRowView {
  filename: string;
  kind: string;
  size: string;
  url: string;
  canSend: boolean;
  sendDisabledReason?: string;
}

export interface PairingView {
  paired: boolean;
  status: string;
  action: string;
}

export interface TakeoverFailureView {
  message: string;
  action: string;
  url: string;
}

interface PopupState {
  resources: ResourceEntry[];
  settings: ExtensionSettings;
  paired: boolean;
  pairing?: { paired: boolean; label?: string };
  takeoverError?: TakeoverFailure | null;
}

interface RuntimeResult {
  ok?: boolean;
}

export function languageFromNavigator(
  value: string | null | undefined,
): ExtensionLanguage {
  return resolveExtensionLanguage(value);
}

function formatSize(size: number, language: ExtensionLanguage): string {
  const locale = language === "zh-CN" ? "zh-CN" : "en-US";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : value >= 100 ? 0 : 1;
  const amount = new Intl.NumberFormat(locale, {
    maximumFractionDigits: digits,
    minimumFractionDigits: 0,
  }).format(value);
  return `${amount} ${units[unit]}`;
}

function isManifestKind(kind: ResourceKind): boolean {
  return kind === "hls" || kind === "dash";
}

export function resourceView(
  resource: ResourceEntry,
  language: ExtensionLanguage,
): ResourceRowView {
  const translate = (key: string, params?: Record<string, string | number>) =>
    translateExtension(language, key, params);
  const manifest = isManifestKind(resource.kind);
  const platform = resource.kind === "video-platform";
  const sendDisabledReason = manifest
    ? translate("popup.manifestUnavailable")
    : platform
      ? translate("overlay.unavailable")
      : undefined;

  return {
    filename: resource.filename,
    kind: translate(`popup.kind.${resource.kind}`),
    size:
      resource.size === null
        ? translate("popup.unknownSize")
        : formatSize(resource.size, language),
    url: redactUrl(resource.url),
    canSend: !manifest && !platform,
    ...(sendDisabledReason === undefined ? {} : { sendDisabledReason }),
  };
}

export function pairingView(paired: boolean, language: ExtensionLanguage): PairingView {
  return {
    paired,
    status: paired
      ? translateExtension(language, "popup.pairedWith", { name: "OneGet" })
      : translateExtension(language, "popup.unpaired"),
    action: translateExtension(language, paired ? "popup.unpair" : "popup.pair"),
  };
}

export function takeoverFailureView(
  failure: TakeoverFailure,
  language: ExtensionLanguage,
): TakeoverFailureView {
  return {
    message: translateExtension(
      language,
      `popup.takeoverFailed.${failure.reason}`,
      { filename: failure.filename },
    ),
    action: translateExtension(language, "popup.takeoverRetry"),
    url: redactUrl(failure.url),
  };
}

function byId<T extends HTMLElement>(id: string): T | null {
  return document.getElementById(id) as T | null;
}

async function sendMessage<T>(message: unknown): Promise<T | undefined> {
  if (typeof chrome === "undefined" || chrome.runtime?.sendMessage === undefined) {
    return undefined;
  }
  try {
    return (await chrome.runtime.sendMessage(message)) as T;
  } catch {
    return undefined;
  }
}

function setText(id: string, value: string): void {
  const element = byId(id);
  if (element !== null) {
    element.textContent = value;
  }
}

function showNotice(message: string): void {
  const notice = byId("notice");
  if (notice === null) {
    return;
  }
  notice.textContent = message;
  notice.hidden = false;
  window.setTimeout(() => {
    notice.hidden = true;
  }, NOTICE_TIMEOUT_MS);
}

function applyStaticTranslations(language: ExtensionLanguage): void {
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n]")) {
    const key = element.dataset.i18n;
    if (key !== undefined) {
      element.textContent = translateExtension(language, key);
    }
  }
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n-placeholder]")) {
    const key = element.dataset.i18nPlaceholder;
    if (key !== undefined) {
      element.setAttribute("placeholder", translateExtension(language, key));
    }
  }
  for (const element of document.querySelectorAll<HTMLElement>("[data-i18n-aria-label]")) {
    const key = element.dataset.i18nAriaLabel;
    if (key !== undefined) {
      element.setAttribute("aria-label", translateExtension(language, key));
    }
  }
}

function renderResources(language: ExtensionLanguage, resources: ResourceEntry[]): void {
  const list = byId<HTMLUListElement>("resources-list");
  const empty = byId("resources-empty");
  const count = byId("resources-count");
  if (list === null) {
    return;
  }

  list.replaceChildren();
  if (count !== null) {
    count.textContent = translateExtension(language, "popup.resourcesCount", {
      count: resources.length,
    });
  }
  if (empty !== null) {
    empty.hidden = resources.length > 0;
  }

  for (const resource of resources) {
    const view = resourceView(resource, language);
    const item = document.createElement("li");
    item.className = "resource";

    const header = document.createElement("div");
    header.className = "resource-header";
    const kind = document.createElement("span");
    kind.className = "resource-kind";
    kind.textContent = view.kind;
    const filename = document.createElement("span");
    filename.className = "resource-filename";
    filename.textContent = view.filename;
    header.append(kind, filename);

    const meta = document.createElement("div");
    meta.className = "resource-meta";
    const size = document.createElement("span");
    size.textContent = view.size;
    const url = document.createElement("span");
    url.className = "resource-url";
    url.textContent = view.url;
    meta.append(size, url);

    const action = document.createElement("button");
    action.type = "button";
    action.className = "resource-send";
    action.textContent = translateExtension(language, "popup.send");
    action.disabled = !view.canSend;
    if (view.sendDisabledReason !== undefined) {
      action.title = view.sendDisabledReason;
    }
    action.addEventListener("click", () => {
      void sendPrefill(resource, language);
    });

    item.append(header, meta, action);
    if (view.sendDisabledReason !== undefined) {
      const reason = document.createElement("p");
      reason.className = "resource-reason";
      reason.textContent = view.sendDisabledReason;
      item.append(reason);
    }
    list.append(item);
  }
}

async function sendPrefill(
  resource: ResourceEntry,
  language: ExtensionLanguage,
): Promise<void> {
  const result = await sendMessage<RuntimeResult>({
    type: "popup/capture-prefill",
    tabId: resource.tabId,
    resource,
  });
  showNotice(
    translateExtension(
      language,
      result?.ok === true ? "popup.captureSent" : "popup.captureFailed",
    ),
  );
}

async function retryTakeoverFailure(
  failure: TakeoverFailure,
  language: ExtensionLanguage,
): Promise<void> {
  const result = await sendMessage<RuntimeResult>({
    type: "popup/capture-prefill",
    url: failure.url,
    resource: { url: failure.url, filename: failure.filename },
  });
  showNotice(
    translateExtension(
      language,
      result?.ok === true ? "popup.captureSent" : "popup.captureFailed",
    ),
  );
  await loadState(language);
}

function renderTakeoverError(
  language: ExtensionLanguage,
  failure: TakeoverFailure | null | undefined,
): void {
  const banner = byId("takeover-error");
  if (banner === null) {
    return;
  }
  if (failure === null || failure === undefined) {
    banner.hidden = true;
    return;
  }
  const view = takeoverFailureView(failure, language);
  setText("takeover-error-message", view.message);
  setText("takeover-error-url", view.url);
  const retry = byId<HTMLButtonElement>("takeover-error-retry");
  if (retry !== null) {
    retry.textContent = view.action;
    retry.onclick = () => {
      void retryTakeoverFailure(failure, language);
    };
  }
  banner.hidden = false;
}

function renderPairing(language: ExtensionLanguage, paired: boolean): void {
  const view = pairingView(paired, language);
  setText("pairing-status", view.status);
  const unpair = byId<HTMLButtonElement>("unpair-button");
  const pairForm = byId<HTMLFormElement>("pair-form");
  if (unpair !== null) {
    unpair.textContent = view.action;
    unpair.hidden = !paired;
  }
  if (pairForm !== null) {
    pairForm.hidden = paired;
  }
}

function renderSettings(settings: ExtensionSettings): void {
  const takeover = byId<HTMLInputElement>("takeover-enabled");
  if (takeover !== null) {
    takeover.checked = settings.takeoverEnabled;
  }
  for (const category of RESOURCE_CATEGORIES) {
    const input = byId<HTMLInputElement>(`category-${category}`);
    if (input !== null) {
      input.checked = settings.categories[category];
    }
  }
  const minimum = byId<HTMLInputElement>("minimum-size");
  if (minimum !== null) {
    minimum.value = String(settings.minimumSizeBytes);
  }
  const blacklist = byId<HTMLTextAreaElement>("blacklist");
  if (blacklist !== null) {
    blacklist.value = settings.blacklistHosts.join("\n");
  }
}

function renderState(language: ExtensionLanguage, state: PopupState): void {
  renderTakeoverError(language, state.takeoverError);
  renderResources(language, state.resources);
  renderPairing(language, state.paired);
  renderSettings(state.settings);
}

async function activeTabId(): Promise<number> {
  if (typeof chrome === "undefined" || chrome.tabs?.query === undefined) {
    return -1;
  }
  try {
    const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    return tabs[0]?.id ?? -1;
  } catch {
    return -1;
  }
}

async function loadState(language: ExtensionLanguage): Promise<void> {
  const tabId = await activeTabId();
  const state = await sendMessage<PopupState>({ type: "popup/get-state", tabId });
  if (state === undefined) {
    setText("pairing-status", translateExtension(language, "popup.serviceUnavailable"));
    return;
  }
  renderState(language, state);
}

function collectSettings(): {
  settings: ExtensionSettings;
  valid: boolean;
} {
  let valid = true;
  const takeover = byId<HTMLInputElement>("takeover-enabled");
  const minimum = byId<HTMLInputElement>("minimum-size");
  const blacklist = byId<HTMLTextAreaElement>("blacklist");

  const parsedMinimum = Number.parseInt(minimum?.value ?? "", 10);
  if (!Number.isFinite(parsedMinimum) || parsedMinimum < 0) {
    valid = false;
  }

  const categories = { ...DEFAULT_SETTINGS.categories };
  for (const category of RESOURCE_CATEGORIES) {
    const input = byId<HTMLInputElement>(`category-${category}`);
    categories[category] = input?.checked ?? DEFAULT_SETTINGS.categories[category];
  }

  const hosts = (blacklist?.value ?? "")
    .split(/\r?\n/)
    .map((entry) => entry.trim())
    .filter((entry) => entry.length > 0);

  const settings = normalizeSettings({
    takeoverEnabled: takeover?.checked ?? false,
    categories,
    minimumSizeBytes: Number.isFinite(parsedMinimum) && parsedMinimum >= 0 ? parsedMinimum : 0,
    blacklistHosts: hosts,
  });
  return { settings, valid };
}

function wireEvents(language: ExtensionLanguage): void {
  const pairForm = byId<HTMLFormElement>("pair-form");
  if (pairForm !== null) {
    pairForm.addEventListener("submit", (event) => {
      event.preventDefault();
      const input = byId<HTMLInputElement>("pairing-code");
      const code = (input?.value ?? "").trim();
      if (code.length === 0) {
        return;
      }
      void (async () => {
        const result = await sendMessage<RuntimeResult>({ type: "popup/pair", code });
        if (result?.ok === true) {
          if (input !== null) {
            input.value = "";
          }
          await loadState(language);
        } else {
          showNotice(translateExtension(language, "popup.pairingFailed"));
        }
      })();
    });
  }

  const unpair = byId<HTMLButtonElement>("unpair-button");
  if (unpair !== null) {
    unpair.addEventListener("click", () => {
      void (async () => {
        await sendMessage({ type: "popup/unpair" });
        await loadState(language);
      })();
    });
  }

  const settingsForm = byId<HTMLFormElement>("settings-form");
  if (settingsForm !== null) {
    settingsForm.addEventListener("submit", (event) => {
      event.preventDefault();
      void (async () => {
        const { settings, valid } = collectSettings();
        await sendMessage({ type: "settings/changed", settings });
        showNotice(
          translateExtension(
            language,
            valid ? "popup.saved" : "popup.invalidSettings",
          ),
        );
      })();
    });
  }
}

function renderCategoryToggles(language: ExtensionLanguage): void {
  const container = byId("category-toggles");
  if (container === null) {
    return;
  }
  container.replaceChildren();
  for (const category of RESOURCE_CATEGORIES) {
    const label = document.createElement("label");
    label.className = "category-toggle";
    const input = document.createElement("input");
    input.type = "checkbox";
    input.id = `category-${category}`;
    input.dataset.category = category;
    const text = document.createElement("span");
    text.textContent = translateExtension(language, CATEGORY_LABEL_KEYS[category]);
    label.append(input, text);
    container.append(label);
  }
}

async function initPopup(): Promise<void> {
  const language = languageFromNavigator(navigator.language);
  applyStaticTranslations(language);
  renderCategoryToggles(language);
  wireEvents(language);
  await loadState(language);
}

if (typeof document !== "undefined") {
  void initPopup();
}

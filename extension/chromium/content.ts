import { resolveExtensionLanguage, translateExtension } from "../shared/i18n";
import type { ExtensionLanguage } from "../shared/i18n";

const INJECT_SOURCE = "oneget-inject";
const OVERLAY_BUTTON_CLASS = "oneget-overlay-button";
const OVERLAY_NOTICE_CLASS = "oneget-overlay-notice";
const HIDE_DELAY_MS = 180;
const NOTICE_DURATION_MS = 4000;
const SCAN_DELAY_MS = 150;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function sendToBackground(message: Record<string, unknown>): void {
  if (typeof chrome === "undefined" || chrome.runtime?.sendMessage === undefined) {
    return;
  }
  try {
    void Promise.resolve(chrome.runtime.sendMessage(message)).catch(() => {
      // The service worker may be restarting; the next observation retries.
    });
  } catch {
    // Sending is best-effort and must never break the page.
  }
}

function nullableString(value: unknown): string | null {
  return typeof value === "string" && value.length > 0 ? value : null;
}

function nullableNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function forwardResource(payload: Record<string, unknown>): void {
  const url = typeof payload.url === "string" ? payload.url : "";
  if (url.length === 0) {
    return;
  }
  sendToBackground({
    type: "content/resource",
    url,
    mime: nullableString(payload.mime),
    size: nullableNumber(payload.size),
    pageUrl: window.location.href,
  });
}

window.addEventListener("message", (event) => {
  if (event.source !== window || !isRecord(event.data)) {
    return;
  }
  const data = event.data;
  if (data.source !== INJECT_SOURCE) {
    return;
  }
  const payload = isRecord(data.payload) ? data.payload : {};
  if (data.kind === "resource") {
    forwardResource(payload);
    return;
  }
  if (data.kind === "spa") {
    sendToBackground({ type: "content/spa", pageUrl: window.location.href });
    observedMedia = new WeakMap();
    scanMediaElements();
  }
});

let observedMedia = new WeakMap<Element, string>();
let scanTimer: number | undefined;

function observeMediaElement(element: Element): void {
  let source = "";
  if (element instanceof HTMLVideoElement || element instanceof HTMLAudioElement) {
    source = element.currentSrc.length > 0 ? element.currentSrc : element.src;
  } else if (element instanceof HTMLSourceElement) {
    source = element.src;
  }
  if (source.length > 0 && observedMedia.get(element) !== source) {
    observedMedia.set(element, source);
    forwardResource({ url: source, mime: null, size: null });
  }
}

function scanMediaElements(): void {
  for (const element of document.querySelectorAll("video, audio, source[src]")) {
    observeMediaElement(element);
  }
}

function scheduleMediaScan(): void {
  if (scanTimer !== undefined) {
    window.clearTimeout(scanTimer);
  }
  scanTimer = window.setTimeout(() => {
    scanTimer = undefined;
    scanMediaElements();
  }, SCAN_DELAY_MS);
}

function isTopFrame(): boolean {
  try {
    return window.top === window.self;
  } catch {
    return false;
  }
}

function installOverlay(language: ExtensionLanguage): void {
  let button: HTMLButtonElement | null = null;
  let notice: HTMLDivElement | null = null;
  let activeVideo: HTMLVideoElement | null = null;
  let hideTimer: number | undefined;
  let noticeTimer: number | undefined;

  function ensureButton(): HTMLButtonElement {
    if (button !== null) {
      return button;
    }
    const element = document.createElement("button");
    element.type = "button";
    element.className = OVERLAY_BUTTON_CLASS;
    element.textContent = "OneGet";
    element.setAttribute("aria-label", translateExtension(language, "overlay.download"));
    element.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      showNotice(translateExtension(language, "overlay.unavailable"));
      hide();
    });
    element.addEventListener("mouseenter", () => {
      if (hideTimer !== undefined) {
        window.clearTimeout(hideTimer);
        hideTimer = undefined;
      }
    });
    element.addEventListener("mouseleave", () => {
      scheduleHide();
    });
    (document.body ?? document.documentElement).append(element);
    button = element;
    return element;
  }

  function ensureNotice(): HTMLDivElement {
    if (notice !== null) {
      return notice;
    }
    const element = document.createElement("div");
    element.className = OVERLAY_NOTICE_CLASS;
    element.setAttribute("role", "status");
    element.setAttribute("aria-live", "polite");
    element.hidden = true;
    (document.body ?? document.documentElement).append(element);
    notice = element;
    return element;
  }

  function showNotice(message: string): void {
    const element = ensureNotice();
    element.textContent = message;
    element.hidden = false;
    if (noticeTimer !== undefined) {
      window.clearTimeout(noticeTimer);
    }
    noticeTimer = window.setTimeout(() => {
      element.hidden = true;
    }, NOTICE_DURATION_MS);
  }

  function position(video: HTMLVideoElement): boolean {
    const rect = video.getBoundingClientRect();
    if (rect.width < 8 || rect.height < 8) {
      return false;
    }
    const element = ensureButton();
    const size = 30;
    const margin = 6;
    const left = Math.min(
      Math.max(margin, rect.right - size - margin),
      Math.max(margin, window.innerWidth - size - margin),
    );
    const top = Math.min(
      Math.max(margin, rect.top + margin),
      Math.max(margin, window.innerHeight - size - margin),
    );
    element.style.left = `${left}px`;
    element.style.top = `${top}px`;
    element.hidden = false;
    return true;
  }

  function show(video: HTMLVideoElement): void {
    if (hideTimer !== undefined) {
      window.clearTimeout(hideTimer);
      hideTimer = undefined;
    }
    if (!position(video)) {
      return;
    }
    activeVideo = video;
  }

  function hide(): void {
    activeVideo = null;
    if (button !== null) {
      button.hidden = true;
    }
  }

  function scheduleHide(): void {
    if (hideTimer !== undefined) {
      window.clearTimeout(hideTimer);
    }
    hideTimer = window.setTimeout(() => {
      hideTimer = undefined;
      hide();
    }, HIDE_DELAY_MS);
  }

  document.addEventListener(
    "mouseover",
    (event) => {
      const target = event.target;
      const video = target instanceof Element ? target.closest("video") : null;
      if (video instanceof HTMLVideoElement) {
        show(video);
      }
    },
    true,
  );

  document.addEventListener(
    "mouseout",
    (event) => {
      const target = event.target;
      const video = target instanceof Element ? target.closest("video") : null;
      if (!(video instanceof HTMLVideoElement)) {
        return;
      }
      const related = event.relatedTarget;
      if (related instanceof Node && (video.contains(related) || button?.contains(related))) {
        return;
      }
      if (activeVideo === video) {
        scheduleHide();
      }
    },
    true,
  );
}

function start(): void {
  const language = resolveExtensionLanguage(navigator.language);
  scanMediaElements();
  const observer = new MutationObserver(() => {
    scheduleMediaScan();
  });
  observer.observe(document.documentElement, {
    childList: true,
    subtree: true,
    attributes: true,
    attributeFilter: ["src"],
  });
  if (isTopFrame()) {
    installOverlay(language);
  }
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", start, { once: true });
} else {
  start();
}

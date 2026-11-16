import type { StorageArea } from "./storage";

export interface BrowserResponseHeader {
  name?: string;
  value?: string;
}

export interface BrowserTab {
  id?: number;
  url?: string;
}

export interface BrowserTabQuery {
  active?: boolean;
  currentWindow?: boolean;
}

export interface BrowserMessageSender {
  tab?: BrowserTab;
  frameId?: number;
}

export type BrowserMessageHandler = (
  message: unknown,
  sender: BrowserMessageSender,
) => unknown;

type MaybePromise<T> = T | Promise<T>;

type Callback<T> = (value: T) => void;

interface RuntimeLike {
  lastError?: { message?: string };
  sendMessage?: (
    message: unknown,
    callback?: Callback<unknown>,
  ) => MaybePromise<unknown>;
  onMessage?: {
    addListener?: (listener: (...args: unknown[]) => unknown) => void;
  };
}

interface TabsLike {
  query?: (
    query: BrowserTabQuery,
    callback?: Callback<BrowserTab[]>,
  ) => MaybePromise<BrowserTab[]>;
}

export interface BrowserCookie {
  name: string;
  value: string;
  domain: string;
  path: string;
  secure: boolean;
  expires?: number;
  httpOnly?: boolean;
  session?: boolean;
  expirationDate?: number;
}

export interface CookieQueryResult {
  cookies: BrowserCookie[];
  permissionMissing: boolean;
  error: boolean;
}

export type CookieQueryLog = (event: string, fields?: Record<string, unknown>) => void;

interface RawBrowserCookie {
  name?: unknown;
  value?: unknown;
  domain?: unknown;
  path?: unknown;
  secure?: unknown;
  session?: unknown;
  expirationDate?: unknown;
}

interface CookiesLike {
  getAll?: (
    details: { url: string },
    callback?: Callback<RawBrowserCookie[]>,
  ) => MaybePromise<RawBrowserCookie[]> | void;
}

interface StorageAreaLike {
  get?: (
    keys: string | string[] | null,
    callback?: Callback<Record<string, unknown>>,
  ) => MaybePromise<Record<string, unknown>>;
  set?: (
    items: Record<string, unknown>,
    callback?: Callback<void>,
  ) => MaybePromise<void>;
  remove?: (
    keys: string | string[],
    callback?: Callback<void>,
  ) => MaybePromise<void>;
}

export interface BrowserApi {
  runtime?: RuntimeLike;
  tabs?: TabsLike;
  cookies?: CookiesLike;
  storage?: {
    local?: StorageAreaLike;
    session?: StorageAreaLike;
  };
}

export type BrowserMessagingStyle = "callback" | "promise";

export interface BrowserAdapterOptions {
  messaging?: BrowserMessagingStyle;
}

export interface BrowserAdapter {
  sendMessage(message: unknown): Promise<unknown>;
  getActiveTab(): Promise<BrowserTab | null>;
  storage(area: "local" | "session"): StorageArea;
  onMessage(handler: BrowserMessageHandler): void;
  responseHeader(
    headers: readonly BrowserResponseHeader[] | undefined,
    name: string,
  ): string | null;
  getCookiesForUrl(url: string, log?: CookieQueryLog): Promise<CookieQueryResult>;
}

// Callback-style APIs declare at least a value plus a completion callback, so
// an arity of 2 or more marks them. Messaging accepts an explicit
// options.messaging override because a host may expose promise-style
// sendMessage with the same declared arity.
function takesCallback(fn: unknown): boolean {
  return typeof fn === "function" && fn.length >= 2;
}

function lastErrorMessage(runtime: RuntimeLike | undefined): string | null {
  const message = runtime?.lastError?.message;
  return typeof message === "string" && message.length > 0 ? message : null;
}

function storageArea(area: StorageAreaLike | undefined): StorageArea {
  return {
    async get(keys) {
      if (area?.get === undefined) {
        return {};
      }
      if (takesCallback(area.get)) {
        return await new Promise<Record<string, unknown>>((resolve, reject) => {
          try {
            area.get?.(keys, (values) => resolve(values));
          } catch (error) {
            reject(error);
          }
        });
      }
      const values = await area.get(keys);
      return values ?? {};
    },
    async set(items) {
      if (area?.set === undefined) {
        return;
      }
      if (takesCallback(area.set)) {
        await new Promise<void>((resolve, reject) => {
          try {
            area.set?.(items, () => resolve());
          } catch (error) {
            reject(error);
          }
        });
        return;
      }
      await area.set(items);
    },
    async remove(keys) {
      if (area?.remove === undefined) {
        return;
      }
      if (takesCallback(area.remove)) {
        await new Promise<void>((resolve, reject) => {
          try {
            area.remove?.(keys, () => resolve());
          } catch (error) {
            reject(error);
          }
        });
        return;
      }
      await area.remove(keys);
    },
  };
}

export function createBrowserAdapter(
  api: BrowserApi,
  options: BrowserAdapterOptions = {},
): BrowserAdapter {
  const runtime = api.runtime;

  function usesCallbackMessaging(): boolean {
    return options.messaging === undefined
      ? takesCallback(runtime?.sendMessage)
      : options.messaging === "callback";
  }

  function sendMessage(message: unknown): Promise<unknown> {
    if (runtime?.sendMessage === undefined) {
      return Promise.resolve(undefined);
    }
    if (usesCallbackMessaging()) {
      return new Promise<unknown>((resolve, reject) => {
        try {
          runtime.sendMessage?.(message, (response) => {
            const failure = lastErrorMessage(runtime);
            if (failure !== null) {
              reject(new Error(failure));
              return;
            }
            resolve(response);
          });
        } catch (error) {
          reject(error);
        }
      });
    }
    return (async () => await runtime.sendMessage?.(message))();
  }

  function getActiveTab(): Promise<BrowserTab | null> {
    if (api.tabs?.query === undefined) {
      return Promise.resolve(null);
    }
    const pick = (tabs: BrowserTab[] | undefined): BrowserTab | null =>
      tabs?.find((tab) => typeof tab.id === "number") ?? null;
    if (takesCallback(api.tabs.query)) {
      return new Promise<BrowserTab | null>((resolve, reject) => {
        try {
          api.tabs?.query?.({ active: true, currentWindow: true }, (tabs) => resolve(pick(tabs)));
        } catch (error) {
          reject(error);
        }
      });
    }
    return (async () => pick(await api.tabs?.query?.({ active: true, currentWindow: true })))();
  }

  function onMessage(handler: BrowserMessageHandler): void {
    const onMessageEvent = runtime?.onMessage;
    if (onMessageEvent?.addListener === undefined) {
      return;
    }
    if (usesCallbackMessaging()) {
      onMessageEvent.addListener((message, sender, sendResponse) => {
        const callback = sendResponse as Callback<unknown>;
        void Promise.resolve(handler(message, (sender ?? {}) as BrowserMessageSender))
          .then((value) => callback(value))
          .catch(() => callback(undefined));
        return true;
      });
      return;
    }
    onMessageEvent.addListener((message, sender) =>
      handler(message, (sender ?? {}) as BrowserMessageSender),
    );
  }

  function responseHeader(
    headers: readonly BrowserResponseHeader[] | undefined,
    name: string,
  ): string | null {
    if (!Array.isArray(headers)) {
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

  function normalizeCookie(raw: RawBrowserCookie): BrowserCookie | null {
    if (typeof raw.name !== "string" || raw.name.length === 0) {
      return null;
    }
    if (typeof raw.value !== "string") {
      return null;
    }
    const expires =
      raw.session === true
        ? undefined
        : typeof raw.expirationDate === "number" && Number.isFinite(raw.expirationDate)
          ? raw.expirationDate
          : undefined;
    return {
      name: raw.name,
      value: raw.value,
      domain: typeof raw.domain === "string" ? raw.domain : "",
      path: typeof raw.path === "string" ? raw.path : "/",
      secure: raw.secure === true,
      expires,
    };
  }

  function permissionFailure(message: string | null): boolean {
    return message !== null && /permission|not\s+allowed/i.test(message);
  }

  async function getCookiesForUrl(
    url: string,
    report?: CookieQueryLog,
  ): Promise<CookieQueryResult> {
    const getAll = api.cookies?.getAll;
    if (typeof getAll !== "function") {
      report?.("cookies.permission_missing", { reason: "api_unavailable" });
      return { cookies: [], permissionMissing: true, error: false };
    }

    try {
      let rawCookies: RawBrowserCookie[] | undefined;
      if (takesCallback(getAll)) {
        rawCookies = await new Promise<RawBrowserCookie[]>((resolve, reject) => {
          try {
            getAll({ url }, (cookies) => {
              const failure = lastErrorMessage(runtime);
              if (failure !== null) {
                reject(new Error(failure));
                return;
              }
              resolve(Array.isArray(cookies) ? cookies : []);
            });
          } catch (error) {
            reject(error);
          }
        });
      } else {
        const result = await getAll({ url });
        rawCookies = Array.isArray(result) ? result : [];
      }

      const cookies: BrowserCookie[] = [];
      for (const raw of Array.isArray(rawCookies) ? rawCookies : []) {
        const cookie = normalizeCookie(raw);
        if (cookie !== null) {
          cookies.push(cookie);
        }
      }
      return { cookies, permissionMissing: false, error: false };
    } catch (error) {
      const message = error instanceof Error ? error.message : null;
      if (permissionFailure(message)) {
        report?.("cookies.permission_missing", { reason: "permission_denied" });
        return { cookies: [], permissionMissing: true, error: false };
      }
      report?.("cookies.query_failed", { reason: "query_failed" });
      return { cookies: [], permissionMissing: false, error: true };
    }
  }

  return {
    sendMessage,
    getActiveTab,
    storage: (area) => storageArea(area === "local" ? api.storage?.local : api.storage?.session),
    onMessage,
    responseHeader,
    getCookiesForUrl,
  };
}

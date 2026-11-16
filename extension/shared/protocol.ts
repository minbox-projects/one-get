export const API_PROTOCOL_VERSION = 2;

const SUPPORTED_PROTOCOL_VERSIONS = [1, 2] as const;

export const API_HOST = "127.0.0.1";

export const API_PORT = 37652;

export const DEFAULT_REQUEST_TIMEOUT_MS = 8000;

export interface ApiErrorBody {
  code: string;
  message: string;
}

export type ApiResult<T> =
  | { ok: true; status: number; data: T }
  | { ok: false; status: number; error: ApiErrorBody };

export interface RequestJsonOptions<TBody = unknown> {
  origin: string;
  path: string;
  method?: "GET" | "POST";
  token?: string;
  body?: TBody;
  fetchImpl?: typeof fetch;
  timeoutMs?: number;
}

export function apiOrigin(port: number = API_PORT): string {
  return `http://${API_HOST}:${port}`;
}

export function isProtocolCompatible(value: unknown): boolean {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const version = (value as { protocolVersion?: unknown }).protocolVersion;
  return (
    typeof version === "number" &&
    (SUPPORTED_PROTOCOL_VERSIONS as readonly number[]).includes(version)
  );
}

export interface ProtocolMismatchGuidance {
  reason: "update_required";
  messageKey: "popup.updateRequired";
  code: "PROTOCOL_MISMATCH";
}

const PROTOCOL_MISMATCH_GUIDANCE: ProtocolMismatchGuidance = {
  reason: "update_required",
  messageKey: "popup.updateRequired",
  code: "PROTOCOL_MISMATCH",
};

export function isProtocolMismatchResult(result: ApiResult<unknown>): boolean {
  return (
    result.ok === false &&
    result.status === 409 &&
    result.error.code === PROTOCOL_MISMATCH_GUIDANCE.code
  );
}

export function protocolMismatchGuidance(
  result: ApiResult<unknown>,
): ProtocolMismatchGuidance | null {
  return isProtocolMismatchResult(result) ? { ...PROTOCOL_MISMATCH_GUIDANCE } : null;
}

function isJsonContentType(value: string | null): boolean {
  return value !== null && value.toLowerCase().includes("json");
}

function parseJson(text: string): unknown {
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return undefined;
  }
}

function parseErrorBody(status: number, text: string): ApiErrorBody {
  const parsed = parseJson(text);
  if (typeof parsed === "object" && parsed !== null) {
    const candidate = parsed as { code?: unknown; message?: unknown };
    if (typeof candidate.code === "string" && typeof candidate.message === "string") {
      return { code: candidate.code, message: candidate.message };
    }
  }
  return { code: "HTTP_ERROR", message: `HTTP ${status}` };
}

export async function requestJson<T>(options: RequestJsonOptions): Promise<ApiResult<T>> {
  const fetchImpl = options.fetchImpl ?? globalThis.fetch;
  const headers: Record<string, string> = { accept: "application/json" };

  if (typeof options.token === "string" && options.token.length > 0) {
    headers.authorization = `Bearer ${options.token}`;
  }

  const init: RequestInit = {
    method: options.method ?? "GET",
    headers,
    credentials: "omit",
    signal: undefined,
  };

  if (options.body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(options.body);
  }

  const controller = new AbortController();
  init.signal = controller.signal;
  const timeoutMs = options.timeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  try {
    const response = await fetchImpl(`${options.origin}${options.path}`, init);
    const text = await response.text();

    if (!response.ok) {
      return {
        ok: false,
        status: response.status,
        error: parseErrorBody(response.status, text),
      };
    }

    if (response.status === 204 || text.trim().length === 0) {
      return { ok: true, status: response.status, data: null as T };
    }

    if (
      isJsonContentType(response.headers.get("content-type")) ||
      response.headers.get("content-type") === null
    ) {
      const parsed = parseJson(text);
      if (parsed !== undefined) {
        return { ok: true, status: response.status, data: parsed as T };
      }
    }

    return {
      ok: false,
      status: response.status,
      error: { code: "INVALID_RESPONSE", message: "invalid response from OneGet" },
    };
  } catch {
    const timedOut = controller.signal.aborted;
    return {
      ok: false,
      status: 0,
      error: timedOut
        ? { code: "TIMEOUT", message: "the request timed out" }
        : { code: "NETWORK_ERROR", message: "the request failed" },
    };
  } finally {
    clearTimeout(timer);
  }
}

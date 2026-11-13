import { describe, expect, it } from "vitest";

import {
  API_HOST,
  API_PORT,
  API_PROTOCOL_VERSION,
  apiOrigin,
  isProtocolCompatible,
  requestJson,
} from "../../shared/protocol";

type FetchArgs = Parameters<typeof fetch>;

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function captureFetch(response: Response) {
  const calls: Array<{ input: FetchArgs[0]; init: FetchArgs[1] | undefined }> = [];
  const fetchImpl = (async (input: FetchArgs[0], init?: FetchArgs[1]) => {
    calls.push({ input, init });
    return response;
  }) as typeof fetch;

  return { fetchImpl, calls };
}

describe("capture API constants", () => {
  it("pins protocol version 1 and the loopback authority", () => {
    expect(API_PROTOCOL_VERSION).toBe(1);
    expect(API_HOST).toBe("127.0.0.1");
    expect(API_PORT).toBe(37652);
    expect(apiOrigin()).toBe("http://127.0.0.1:37652");
    expect(apiOrigin(4000)).toBe("http://127.0.0.1:4000");
  });

  it("accepts only the compatible protocol version", () => {
    expect(isProtocolCompatible({ protocolVersion: 1 })).toBe(true);
    expect(isProtocolCompatible({ protocolVersion: 2 })).toBe(false);
    expect(isProtocolCompatible({ protocolVersion: "1" })).toBe(false);
    expect(isProtocolCompatible({})).toBe(false);
  });
});

describe("capture API requests", () => {
  it("sends a JSON POST with accept, bearer, and an omitted cookie jar", async () => {
    const { fetchImpl, calls } = captureFetch(jsonResponse({ accepted: 1 }));
    const body = { protocolVersion: 1, intent: "prefill", items: [] };

    const result = await requestJson<{ accepted: number }>({
      origin: apiOrigin(),
      path: "/capture",
      method: "POST",
      token: "abc123",
      body,
      fetchImpl,
    });

    expect(result).toEqual({ ok: true, status: 200, data: { accepted: 1 } });
    expect(String(calls[0].input)).toBe("http://127.0.0.1:37652/capture");

    const init = calls[0].init;
    const headers = new Headers(init?.headers);
    expect(init?.method).toBe("POST");
    expect(init?.credentials).toBe("omit");
    expect(init?.body).toBe(JSON.stringify(body));
    expect(headers.get("content-type")).toBe("application/json");
    expect(headers.get("accept")).toBe("application/json");
    expect(headers.get("authorization")).toBe("Bearer abc123");
    expect(headers.get("cookie")).toBeNull();
  });

  it("omits authorization and content type on an unauthenticated GET", async () => {
    const { fetchImpl, calls } = captureFetch(jsonResponse({ ok: true }));

    await requestJson({
      origin: apiOrigin(),
      path: "/health",
      method: "GET",
      fetchImpl,
    });

    const init = calls[0].init;
    const headers = new Headers(init?.headers);
    expect(init?.credentials).toBe("omit");
    expect(init?.body).toBeUndefined();
    expect(headers.get("accept")).toBe("application/json");
    expect(headers.get("authorization")).toBeNull();
    expect(headers.get("content-type")).toBeNull();
    expect(headers.get("cookie")).toBeNull();
  });

  it("aborts an over-budget request and reports a TIMEOUT with status zero", async () => {
    const fetchImpl = (async (_input: FetchArgs[0], init?: FetchArgs[1]) => {
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener("abort", () => {
          const error = new Error("aborted");
          error.name = "AbortError";
          reject(error);
        });
      });
    }) as typeof fetch;

    const result = await requestJson({
      origin: apiOrigin(),
      path: "/slow",
      fetchImpl,
      timeoutMs: 10,
    });

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.status).toBe(0);
      expect(result.error.code).toBe("TIMEOUT");
    }
  });

  it("rejects a non-JSON success body as an invalid response", async () => {
    const { fetchImpl } = captureFetch(
      new Response("<html>not json</html>", {
        status: 200,
        headers: { "content-type": "text/html" },
      }),
    );

    const result = await requestJson({
      origin: apiOrigin(),
      path: "/health",
      fetchImpl,
    });

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.error.code).toBe("INVALID_RESPONSE");
    }
  });

  it("propagates a typed HTTP error code and status", async () => {
    const { fetchImpl } = captureFetch(
      jsonResponse({ code: "FORBIDDEN", message: "origin rejected" }, 403),
    );

    const result = await requestJson({
      origin: apiOrigin(),
      path: "/capture",
      method: "POST",
      body: {},
      fetchImpl,
    });

    expect(result).toEqual({
      ok: false,
      status: 403,
      error: { code: "FORBIDDEN", message: "origin rejected" },
    });
  });

  it("maps a non-JSON HTTP error body to HTTP_ERROR", async () => {
    const { fetchImpl } = captureFetch(new Response("bad gateway", { status: 502 }));

    const result = await requestJson({
      origin: apiOrigin(),
      path: "/capture",
      method: "POST",
      body: {},
      fetchImpl,
    });

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.status).toBe(502);
      expect(result.error.code).toBe("HTTP_ERROR");
    }
  });

  it("propagates a protocol mismatch from a 409 response", async () => {
    const { fetchImpl } = captureFetch(
      jsonResponse({ code: "PROTOCOL_MISMATCH", message: "update OneGet" }, 409),
    );

    const result = await requestJson({
      origin: apiOrigin(),
      path: "/capture",
      method: "POST",
      body: { protocolVersion: 2 },
      fetchImpl,
    });

    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.status).toBe(409);
      expect(result.error.code).toBe("PROTOCOL_MISMATCH");
    }
  });
});

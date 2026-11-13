import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ProbeResult } from "../../types/downloads";

const { probeDownload } = vi.hoisted(() => ({ probeDownload: vi.fn() }));

vi.mock("../../lib/downloads", () => ({ probeDownload }));

import { useProbe } from "./useProbe";

const probeResult: ProbeResult = {
  finalUrl: "https://cdn.example.com/file.bin",
  totalBytes: 2048,
  rangeSupported: true,
  filename: "file.bin",
  contentType: "application/octet-stream",
  etag: "\"etag\"",
  lastModified: null,
};

describe("useProbe", () => {
  beforeEach(() => {
    probeDownload.mockReset();
  });

  it("starts idle and moves through detecting to success", async () => {
    let resolveProbe!: (value: unknown) => void;
    probeDownload.mockReturnValue(
      new Promise((resolve) => {
        resolveProbe = resolve;
      }),
    );

    const { result } = renderHook(() => useProbe());
    expect(result.current.state).toEqual({ status: "idle" });

    act(() => {
      void result.current.probe("https://example.com/file.bin", false);
    });

    expect(result.current.state).toEqual({ status: "detecting" });
    expect(probeDownload).toHaveBeenCalledWith({
      url: "https://example.com/file.bin",
      allowHtml: false,
      extraHeaders: [],
    });

    await act(async () => {
      resolveProbe({ ok: true, data: probeResult });
    });

    expect(result.current.state).toEqual({
      status: "success",
      result: probeResult,
    });
  });

  it("passes the HTML allowance to the probe request", async () => {
    probeDownload.mockResolvedValue({ ok: true, data: probeResult });

    const { result } = renderHook(() => useProbe());
    await act(async () => {
      await result.current.probe("https://example.com/page.html", true);
    });

    expect(probeDownload).toHaveBeenCalledWith({
      url: "https://example.com/page.html",
      allowHtml: true,
      extraHeaders: [],
    });
    await waitFor(() => {
      expect(result.current.state.status).toBe("success");
    });
  });

  it("exposes probe errors and resets back to idle", async () => {
    probeDownload.mockResolvedValue({
      ok: false,
      error: { code: "http_error", message: "The server returned 404." },
    });

    const { result } = renderHook(() => useProbe());
    await act(async () => {
      await result.current.probe("https://example.com/missing.bin", false);
    });

    expect(result.current.state).toEqual({
      status: "error",
      error: { code: "http_error", message: "The server returned 404." },
    });

    act(() => {
      result.current.reset();
    });

    expect(result.current.state).toEqual({ status: "idle" });
  });
});

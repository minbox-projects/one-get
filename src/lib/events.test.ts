import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PendingPrefill } from "../types/pairing";

const { listen, isTauriRuntime } = vi.hoisted(() => ({
  listen: vi.fn(),
  isTauriRuntime: vi.fn(),
}));

vi.mock("./tauri", () => ({ isTauriRuntime }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

import { CAPTURE_PREFILL_EVENT, subscribeToCapturePrefill } from "./events";

type CaptureHandler = (event: { payload: PendingPrefill }) => void;

let emit: CaptureHandler | undefined;
let stop: ReturnType<typeof vi.fn>;

function pendingPrefill(): PendingPrefill {
  return {
    items: [
      {
        url: "https://files.example.test/captured-video.mp4",
        filename: "captured-video.mp4",
        referer: "https://page.example.test/watch",
        mime: "video/mp4",
        size: 2048,
        headers: [],
      },
    ],
    receivedAtMs: 1_700_000_000_000,
  };
}

beforeEach(() => {
  emit = undefined;
  stop = vi.fn(() => {
    emit = undefined;
  });
  listen.mockReset();
  isTauriRuntime.mockReset();
  isTauriRuntime.mockReturnValue(true);
  listen.mockImplementation(async (_event: string, handler: CaptureHandler) => {
    emit = handler;
    return stop;
  });
});

describe("capture prefill event", () => {
  it("subscribes to the capture prefill event and delivers its payload", async () => {
    const handler = vi.fn();

    subscribeToCapturePrefill(handler);

    await vi.waitFor(() => expect(emit).toBeTypeOf("function"));
    expect(listen).toHaveBeenCalledTimes(1);
    expect(listen.mock.calls[0]?.[0]).toBe(CAPTURE_PREFILL_EVENT);

    const payload = pendingPrefill();
    emit?.({ payload });

    expect(handler).toHaveBeenCalledWith(payload);
  });

  it("stops delivery when unsubscribed", async () => {
    const handler = vi.fn();
    const unsubscribe = subscribeToCapturePrefill(handler);

    await vi.waitFor(() => expect(emit).toBeTypeOf("function"));
    unsubscribe();

    expect(stop).toHaveBeenCalledTimes(1);
    expect(emit).toBeUndefined();
    expect(handler).not.toHaveBeenCalled();
  });

  it("stops a late listener that resolves after unsubscribe", async () => {
    let resolveListen!: (value: unknown) => void;
    listen.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveListen = resolve;
        }),
    );
    const handler = vi.fn();
    const unsubscribe = subscribeToCapturePrefill(handler);

    expect(listen).toHaveBeenCalledTimes(1);
    unsubscribe();
    resolveListen(stop);

    await vi.waitFor(() => expect(stop).toHaveBeenCalled());
    expect(handler).not.toHaveBeenCalled();
  });

  it("is a no-op when the Tauri runtime is unavailable", async () => {
    isTauriRuntime.mockReturnValue(false);
    const handler = vi.fn();

    const unsubscribe = subscribeToCapturePrefill(handler);
    unsubscribe();
    await new Promise((resolve) => setTimeout(resolve, 10));

    expect(listen).not.toHaveBeenCalled();
    expect(handler).not.toHaveBeenCalled();
  });
});

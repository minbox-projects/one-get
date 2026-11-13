import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AppError, IpcResult } from "../../types/app";
import type {
  PairingCodeView,
  PairingStatus,
} from "../../types/pairing";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("../../lib/tauri", () => ({ invokeCommand }));

import { usePairing } from "./usePairing";

const NOW = 1_700_000_000_000;

type Handler = (args?: Record<string, unknown>) => unknown;

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

function status(overrides: Partial<PairingStatus> = {}): PairingStatus {
  return {
    available: true,
    reason: null,
    protocolVersion: 1,
    code: null,
    clients: [],
    ...overrides,
  };
}

function pairingCode(overrides: Partial<PairingCodeView> = {}): PairingCodeView {
  return { code: "12345678", expiresAtMs: NOW + 600_000, ...overrides };
}

function mockIpc(handlers: Record<string, Handler>): void {
  invokeCommand.mockImplementation(
    async (command: string, args?: Record<string, unknown>) => {
      const handler = handlers[command];
      if (handler) {
        return handler(args);
      }
      return {
        ok: false,
        error: { code: "unexpected_command", message: command },
      };
    },
  );
}

function commandNames(): string[] {
  return invokeCommand.mock.calls.map(([command]) => command as string);
}

function statusCalls(): number {
  return commandNames().filter((command) => command === "pairing_status").length;
}

async function flush(ms = 0): Promise<void> {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

beforeEach(() => {
  invokeCommand.mockReset();
  vi.useFakeTimers({ now: NOW });
});

afterEach(() => {
  vi.useRealTimers();
});

describe("usePairing", () => {
  it("loads pairing status on mount and clears loading", async () => {
    const initial = status({ clients: [] });
    mockIpc({ pairing_status: () => ({ ok: true, data: initial }) });

    const { result } = renderHook(() => usePairing());
    expect(result.current.loading).toBe(true);

    await flush();

    expect(result.current.loading).toBe(false);
    expect(result.current.status).toEqual(initial);
    expect(result.current.error).toBeNull();
    expect(result.current.remainingSeconds).toBe(0);
    expect(commandNames()).toContain("pairing_status");
  });

  it("refreshes about every two seconds while mounted and stops after unmount", async () => {
    mockIpc({ pairing_status: () => ({ ok: true, data: status() }) });

    const { unmount } = renderHook(() => usePairing());
    await flush();
    expect(statusCalls()).toBe(1);

    await flush(4_100);
    expect(statusCalls()).toBeGreaterThanOrEqual(3);

    unmount();
    const callsAfterUnmount = statusCalls();
    await flush(6_000);
    expect(statusCalls()).toBe(callsAfterUnmount);
  });

  it("counts down once per second from the code expiry and reaches zero", async () => {
    const code = pairingCode({ expiresAtMs: NOW + 3_000 });
    mockIpc({ pairing_status: () => ({ ok: true, data: status({ code }) }) });

    const { result } = renderHook(() => usePairing());
    await flush();

    const initial = result.current.remainingSeconds;
    expect(initial).toBeGreaterThan(0);
    expect(initial).toBeLessThanOrEqual(3);

    await flush(1_000);
    expect(result.current.remainingSeconds).toBeLessThan(initial);
    expect(result.current.remainingSeconds).toBeGreaterThan(0);

    await flush(2_500);
    expect(result.current.remainingSeconds).toBe(0);
  });

  it("sets busy while generating and exposes the returned code even when status lags", async () => {
    const generated = pairingCode();
    const gate = deferred<IpcResult<PairingCodeView>>();
    mockIpc({
      pairing_status: () => ({ ok: true, data: status() }),
      pairing_generate_code: () => gate.promise,
    });

    const { result } = renderHook(() => usePairing());
    await flush();

    let pending: Promise<void> | undefined;
    act(() => {
      pending = result.current.generate();
    });
    await flush();
    expect(result.current.busy).toBe(true);

    await act(async () => {
      gate.resolve({ ok: true, data: generated });
      await pending;
    });

    expect(result.current.busy).toBe(false);
    expect(result.current.status?.code).toEqual(generated);
    expect(statusCalls()).toBeGreaterThanOrEqual(2);
  });

  it("revokes a client, refreshes on success, and returns the result", async () => {
    const gate = deferred<IpcResult<boolean>>();
    let statusCount = 0;
    mockIpc({
      pairing_status: () => {
        statusCount += 1;
        return { ok: true, data: status() };
      },
      pairing_revoke: () => gate.promise,
    });

    const { result } = renderHook(() => usePairing());
    await flush();
    const countBeforeRevoke = statusCount;

    let pending: Promise<boolean> | undefined;
    act(() => {
      pending = result.current.revoke("client-1");
    });
    await flush();
    expect(result.current.busy).toBe(true);

    let outcome = false;
    await act(async () => {
      gate.resolve({ ok: true, data: true });
      outcome = await (pending as Promise<boolean>);
    });

    expect(outcome).toBe(true);
    expect(result.current.busy).toBe(false);
    expect(statusCount).toBeGreaterThan(countBeforeRevoke);
    expect(invokeCommand).toHaveBeenCalledWith("pairing_revoke", {
      clientId: "client-1",
    });
  });

  it("surfaces a typed wrapper failure as the hook error", async () => {
    const failure: AppError = {
      code: "pairing_locked",
      message: "Pairing is locked.",
    };
    mockIpc({
      pairing_status: () => ({ ok: true, data: status() }),
      pairing_generate_code: () => ({ ok: false, error: failure }),
    });

    const { result } = renderHook(() => usePairing());
    await flush();

    await act(async () => {
      await result.current.generate();
    });

    expect(result.current.error).toEqual(failure);
    expect(result.current.busy).toBe(false);
  });

  it("ignores a late generate response after unmount", async () => {
    const generated = pairingCode();
    const gate = deferred<IpcResult<PairingCodeView>>();
    let statusCount = 0;
    mockIpc({
      pairing_status: () => {
        statusCount += 1;
        return { ok: true, data: status() };
      },
      pairing_generate_code: () => gate.promise,
    });

    const { result, unmount } = renderHook(() => usePairing());
    await flush();
    const statusCallsBeforeUnmount = statusCount;

    let pending: Promise<void> | undefined;
    act(() => {
      pending = result.current.generate();
    });
    unmount();

    await act(async () => {
      gate.resolve({ ok: true, data: generated });
      await pending;
    });
    await flush(6_000);

    expect(statusCount).toBe(statusCallsBeforeUnmount);
  });
});

import { beforeEach, describe, expect, it, vi } from "vitest";

import type { IpcResult } from "../types/app";
import type {
  PairedClientView,
  PairingCodeView,
  PairingStatus,
  PendingPrefill,
} from "../types/pairing";

const { invokeCommand } = vi.hoisted(() => ({ invokeCommand: vi.fn() }));

vi.mock("./tauri", () => ({ invokeCommand }));

import {
  generatePairingCode,
  getPairingStatus,
  listPairedClients,
  revokePairing,
  takePendingPrefill,
} from "./pairing";

function pairingStatus(): PairingStatus {
  return {
    available: true,
    reason: null,
    protocolVersion: 1,
    code: null,
    clients: [],
  };
}

function pairingCode(): PairingCodeView {
  return { code: "12345678", expiresAtMs: 1_700_000_000_000 };
}

function pairedClient(): PairedClientView {
  return {
    id: "client-1",
    label: "Laptop",
    origin: "chrome-extension://abcdefghijklmnop",
    createdAtMs: 1_700_000_000_000,
    lastUsedAtMs: 1_700_000_100_000,
  };
}

beforeEach(() => {
  invokeCommand.mockReset();
  invokeCommand.mockResolvedValue({
    ok: false,
    error: { code: "unexpected_command", message: "unexpected command" },
  });
});

describe("pairing wrappers", () => {
  it("loads pairing status through the pairing_status command without arguments", async () => {
    const data = pairingStatus();
    invokeCommand.mockResolvedValueOnce({ ok: true, data });

    const result = await getPairingStatus();

    expect(result).toEqual({ ok: true, data });
    expect(invokeCommand).toHaveBeenCalledWith("pairing_status");
  });

  it("generates a pairing code through the pairing_generate_code command", async () => {
    const data = pairingCode();
    invokeCommand.mockResolvedValueOnce({ ok: true, data });

    const result = await generatePairingCode();

    expect(result).toEqual({ ok: true, data });
    expect(invokeCommand).toHaveBeenCalledWith("pairing_generate_code");
  });

  it("lists paired clients through the pairing_list_clients command", async () => {
    const data = [pairedClient()];
    invokeCommand.mockResolvedValueOnce({ ok: true, data });

    const result = await listPairedClients();

    expect(result).toEqual({ ok: true, data });
    expect(invokeCommand).toHaveBeenCalledWith("pairing_list_clients");
  });

  it("revokes one client by id through the pairing_revoke command", async () => {
    invokeCommand.mockResolvedValueOnce({ ok: true, data: true });

    const result = await revokePairing("client-1");

    expect(result).toEqual({ ok: true, data: true });
    expect(invokeCommand).toHaveBeenCalledWith("pairing_revoke", {
      clientId: "client-1",
    });
  });

  it("takes the pending prefill through the pairing_take_pending_prefill command", async () => {
    const data: PendingPrefill = {
      items: [
        {
          url: "https://example.com/file.bin",
          filename: "file.bin",
          referer: "https://example.com/page",
          mime: "application/octet-stream",
          size: 1024,
          headers: [{ name: "Referer", value: "https://example.com/page" }],
        },
      ],
      receivedAtMs: 1_700_000_000_000,
    };
    invokeCommand.mockResolvedValueOnce({ ok: true, data });

    const result = await takePendingPrefill();

    expect(result).toEqual({ ok: true, data });
    expect(invokeCommand).toHaveBeenCalledWith("pairing_take_pending_prefill");
  });

  it("returns a typed wrapper failure unchanged", async () => {
    const failure: IpcResult<boolean> = {
      ok: false,
      error: { code: "pairing_revoke_failed", message: "Revocation failed." },
    };
    invokeCommand.mockResolvedValueOnce(failure);

    await expect(revokePairing("client-1")).resolves.toEqual(failure);
  });
});

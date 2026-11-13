import type { IpcResult } from "../types/app";
import type {
  PairedClientView,
  PairingCodeView,
  PairingStatus,
  PendingPrefill,
} from "../types/pairing";

import { invokeCommand } from "./tauri";

/** Reads browser-integration availability, the active code and paired clients. */
export function getPairingStatus(): Promise<IpcResult<PairingStatus>> {
  return invokeCommand<PairingStatus>("pairing_status");
}

/** Creates a fresh one-time pairing code, replacing any earlier code. */
export function generatePairingCode(): Promise<IpcResult<PairingCodeView>> {
  return invokeCommand<PairingCodeView>("pairing_generate_code");
}

export function listPairedClients(): Promise<IpcResult<PairedClientView[]>> {
  return invokeCommand<PairedClientView[]>("pairing_list_clients");
}

/** Revokes one paired client by identifier. */
export function revokePairing(clientId: string): Promise<IpcResult<boolean>> {
  return invokeCommand<boolean>("pairing_revoke", { clientId });
}

/** Takes the pending capture prefill, leaving nothing behind for a second read. */
export function takePendingPrefill(): Promise<IpcResult<PendingPrefill | null>> {
  return invokeCommand<PendingPrefill | null>("pairing_take_pending_prefill");
}

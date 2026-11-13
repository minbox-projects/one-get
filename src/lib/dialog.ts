import { open } from "@tauri-apps/plugin-dialog";

import type { IpcResult } from "../types/app";

import { isTauriRuntime } from "./tauri";

const RUNTIME_UNAVAILABLE_MESSAGE =
  "Tauri runtime is unavailable in this environment.";

/**
 * Opens the native directory picker. A cancelled picker resolves to `null` so
 * callers can keep the current destination, and a missing desktop runtime
 * returns a typed error instead of throwing.
 */
export async function pickDirectory(): Promise<IpcResult<string | null>> {
  if (!isTauriRuntime()) {
    return {
      ok: false,
      error: {
        code: "runtime_unavailable",
        message: RUNTIME_UNAVAILABLE_MESSAGE,
      },
    };
  }

  try {
    const selected = await open({ directory: true, multiple: false });
    return { ok: true, data: typeof selected === "string" ? selected : null };
  } catch (error) {
    return {
      ok: false,
      error: {
        code: "dialog_failed",
        message:
          error instanceof Error && error.message.length > 0
            ? error.message
            : "The directory picker could not be opened.",
      },
    };
  }
}

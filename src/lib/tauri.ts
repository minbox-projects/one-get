import type { AppError, IpcResult } from "../types/app";

export type { AppError, AppInfo, IpcResult, Settings } from "../types/app";

const RUNTIME_UNAVAILABLE_MESSAGE =
  "Tauri runtime is unavailable in this environment.";

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function toAppError(error: unknown): AppError {
  if (typeof error === "string" && error.length > 0) {
    return { code: "invoke_error", message: error };
  }

  if (typeof error === "object" && error !== null) {
    const candidate = error as { code?: unknown; message?: unknown };
    if (typeof candidate.code === "string" && candidate.code.length > 0) {
      return {
        code: candidate.code,
        message:
          typeof candidate.message === "string"
            ? candidate.message
            : "The command failed.",
      };
    }
    if (typeof candidate.message === "string" && candidate.message.length > 0) {
      return { code: "invoke_error", message: candidate.message };
    }
  }

  return { code: "invoke_error", message: "The command failed." };
}

export async function invokeCommand<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<IpcResult<T>> {
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
    const { invoke } = await import("@tauri-apps/api/core");
    const data = await invoke<T>(command, args);
    return { ok: true, data };
  } catch (error) {
    return { ok: false, error: toAppError(error) };
  }
}

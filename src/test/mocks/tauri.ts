import { vi } from "vitest";

export type AppError = { code: string; message: string };
export type IpcResult<T> =
  | { ok: true; data: T }
  | { ok: false; error: AppError };

export const ok = <T>(data: T): IpcResult<T> => ({ ok: true, data });

export const err = (code: string, message: string): IpcResult<never> => ({
  ok: false,
  error: { code, message },
});

export const runtimeUnavailable = (
  message = "Tauri runtime is unavailable in this environment.",
): IpcResult<never> => err("runtime_unavailable", message);

export const invokeCommand = vi.fn();
export const isTauriRuntime = vi.fn(() => false);

export function resetTauriMocks(): void {
  invokeCommand.mockReset();
  isTauriRuntime.mockReset();
  isTauriRuntime.mockReturnValue(false);
}

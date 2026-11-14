import type { IpcResult } from "../types/app";
import type { ComponentName, ComponentStatusView } from "../types/kernel";

import { invokeCommand } from "./tauri";

/** Reads the current status of every installed or available component. */
export function getKernelStatus(): Promise<IpcResult<ComponentStatusView[]>> {
  return invokeCommand<ComponentStatusView[]>("kernel_status");
}

/** Re-fetches and re-verifies the signed component manifest. */
export function refreshKernelManifest(): Promise<IpcResult<null>> {
  return invokeCommand<null>("kernel_manifest_refresh");
}

/** Starts installing a component that is not active on this target. */
export function installComponent(
  component: ComponentName,
): Promise<IpcResult<null>> {
  return invokeCommand<null>("kernel_install", { component });
}

/** Starts installing a strictly newer component version. */
export function updateComponent(
  component: ComponentName,
): Promise<IpcResult<null>> {
  return invokeCommand<null>("kernel_update", { component });
}

/** Switches the active version to an already installed earlier version. */
export function rollbackComponent(
  component: ComponentName,
): Promise<IpcResult<null>> {
  return invokeCommand<null>("kernel_rollback", { component });
}

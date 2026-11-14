import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ComponentStatusView } from "../../types/kernel";

vi.mock("../../lib/tauri", async () => await import("../../test/mocks/tauri"));

import {
  getKernelStatus,
  installComponent,
  refreshKernelManifest,
  rollbackComponent,
  updateComponent,
} from "../../lib/kernel";
import {
  err,
  invokeCommand,
  ok,
  resetTauriMocks,
} from "../../test/mocks/tauri";

beforeEach(() => {
  vi.clearAllMocks();
  resetTauriMocks();
});

function componentViews(): ComponentStatusView[] {
  return [
    {
      component: "yt-dlp",
      state: "update_available",
      activeVersion: "2025.10.22",
      availableVersion: "2025.11.01",
      installedVersions: ["2025.10.22", "2025.09.26"],
      target: "darwin-arm64",
      rollbackVersion: "2025.09.26",
      operation: { phase: "downloading", percent: 42 },
      error: null,
    },
    {
      component: "ffmpeg",
      state: "installed",
      activeVersion: "7.1",
      availableVersion: null,
      installedVersions: ["7.1"],
      target: "darwin-arm64",
      rollbackVersion: null,
      operation: null,
      error: null,
    },
    {
      component: "deno",
      state: "tampered",
      activeVersion: "2.1.5",
      availableVersion: null,
      installedVersions: ["2.1.5"],
      target: "darwin-arm64",
      rollbackVersion: "2.1.4",
      operation: null,
      error: {
        code: "COMPONENT_CHECKSUM_MISMATCH",
        message: "The installed component failed verification.",
      },
    },
  ];
}

describe("kernel api wrappers", () => {
  it("kernel_status_wrapper_returns_typed_component_views", async () => {
    const views = componentViews();
    invokeCommand.mockResolvedValue(ok(views));

    const result = await getKernelStatus();

    expect(invokeCommand).toHaveBeenCalledWith("kernel_status");
    expect(result).toEqual({ ok: true, data: views });
    expect(result.ok && result.data[0]?.operation).toEqual({
      phase: "downloading",
      percent: 42,
    });
    expect(result.ok && result.data[2]?.error).toEqual({
      code: "COMPONENT_CHECKSUM_MISMATCH",
      message: "The installed component failed verification.",
    });
  });

  it("component_actions_invoke_the_matching_command_with_the_component", async () => {
    invokeCommand.mockResolvedValue(ok(null));

    const actions = [
      ["kernel_install", installComponent],
      ["kernel_update", updateComponent],
      ["kernel_rollback", rollbackComponent],
    ] as const;

    for (const [command, action] of actions) {
      const result = await action("ffmpeg");

      expect(invokeCommand).toHaveBeenCalledWith(command, {
        component: "ffmpeg",
      });
      expect(result).toEqual({ ok: true, data: null });
    }
  });

  it("manifest_refresh_uses_its_command_and_propagates_errors", async () => {
    invokeCommand.mockResolvedValue(ok(null));

    const success = await refreshKernelManifest();

    expect(invokeCommand).toHaveBeenCalledWith("kernel_manifest_refresh");
    expect(success).toEqual({ ok: true, data: null });

    const failure = err(
      "COMPONENT_MANIFEST_UNAVAILABLE",
      "The component manifest could not be loaded.",
    );
    invokeCommand.mockResolvedValue(failure);

    const result = await refreshKernelManifest();

    expect(invokeCommand).toHaveBeenLastCalledWith("kernel_manifest_refresh");
    expect(result).toEqual(failure);
  });
});

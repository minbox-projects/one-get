import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider, translate } from "../../i18n";
import type {
  ComponentOperation,
  ComponentStatusView,
} from "../../types/kernel";

import { ComponentsSection } from "./ComponentsSection";
import { MissingComponentAction } from "./MissingComponentAction";

const {
  getKernelStatus,
  installComponent,
  updateComponent,
  rollbackComponent,
} = vi.hoisted(() => ({
  getKernelStatus: vi.fn(),
  installComponent: vi.fn(),
  updateComponent: vi.fn(),
  rollbackComponent: vi.fn(),
}));

vi.mock("../../lib/kernel", () => ({
  getKernelStatus,
  installComponent,
  updateComponent,
  rollbackComponent,
  refreshKernelManifest: vi.fn(),
}));

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

const IDLE = { ok: true as const, data: null };

function statusView(
  overrides: Partial<ComponentStatusView> = {},
): ComponentStatusView {
  return {
    component: "yt-dlp",
    state: "installed",
    activeVersion: "2025.11.01",
    availableVersion: null,
    installedVersions: ["2025.11.01"],
    target: "macos-arm64",
    rollbackVersion: null,
    operation: null,
    error: null,
    ...overrides,
  };
}

function downloadingOperation(percent: number): ComponentOperation {
  return { phase: "downloading", percent };
}

function mockStatus(views: ComponentStatusView[]) {
  getKernelStatus.mockResolvedValue({ ok: true, data: views });
}

function renderSection() {
  return render(
    <I18nProvider language="en-US">
      <ComponentsSection />
    </I18nProvider>,
  );
}

beforeEach(() => {
  getKernelStatus.mockReset();
  installComponent.mockReset();
  updateComponent.mockReset();
  rollbackComponent.mockReset();
  mockStatus([statusView()]);
  installComponent.mockResolvedValue(IDLE);
  updateComponent.mockResolvedValue(IDLE);
  rollbackComponent.mockResolvedValue(IDLE);
});

describe("ComponentsSection", () => {
  it("components_section_renders_states_versions_and_operation", async () => {
    mockStatus([
      statusView({
        component: "yt-dlp",
        state: "installed",
        activeVersion: "2025.11.01",
        availableVersion: null,
        installedVersions: ["2025.10.01", "2025.11.01"],
        target: "macos-arm64",
        rollbackVersion: "2025.10.01",
        operation: downloadingOperation(42),
      }),
      statusView({
        component: "ffmpeg",
        state: "update_available",
        activeVersion: "7.1",
        availableVersion: "7.2",
        installedVersions: ["7.1"],
        target: "macos-arm64",
      }),
      statusView({
        component: "deno",
        state: "manifest_unavailable",
        activeVersion: null,
        availableVersion: null,
        installedVersions: [],
        target: "macos-arm64",
      }),
    ]);
    renderSection();

    await screen.findByText("ffmpeg");
    const text = document.body.textContent ?? "";
    expect(text).toContain(en["kernel.state.installed"]);
    expect(text).toContain(en["kernel.state.update_available"]);
    expect(text).toContain(en["kernel.state.manifest_unavailable"]);
    expect(text).toContain(en["kernel.activeVersion"]);
    expect(text).toContain(en["kernel.availableVersion"]);
    expect(text).toContain(en["kernel.installedVersions"]);
    expect(text).toContain(en["kernel.rollbackTo"]);
    expect(text).toContain(en["kernel.platform"]);
    expect(text).toContain(en["kernel.none"]);
    expect(text).toContain("2025.11.01");
    expect(text).toContain("2025.10.01");
    expect(text).toContain("7.1");
    expect(text).toContain("7.2");
    expect(text).toContain("macos-arm64");
    expect(text).toMatch(/42\s*%/);
  });

  it("component_actions_call_the_matching_command_and_refresh", async () => {
    const user = userEvent.setup();
    mockStatus([
      statusView({ component: "yt-dlp", rollbackVersion: "2025.10.01" }),
      statusView({
        component: "deno",
        state: "not_installed",
        activeVersion: null,
        installedVersions: [],
        rollbackVersion: null,
      }),
    ]);
    const first = renderSection();

    const install = await screen.findByRole("button", {
      name: en["kernel.install"],
    });
    await user.click(install);
    expect(installComponent).toHaveBeenCalledTimes(1);
    expect(installComponent).toHaveBeenCalledWith("deno");
    expect(rollbackComponent).not.toHaveBeenCalled();
    await waitFor(() => expect(getKernelStatus).toHaveBeenCalledTimes(2));

    const rollback = screen.getByRole("button", {
      name: en["kernel.rollback"],
    });
    await user.click(rollback);
    expect(rollbackComponent).toHaveBeenCalledTimes(1);
    expect(rollbackComponent).toHaveBeenCalledWith("yt-dlp");
    await waitFor(() => expect(getKernelStatus).toHaveBeenCalledTimes(3));
    expect(document.body.textContent).toContain(en["kernel.hint.notInstalled"]);
    first.unmount();
    getKernelStatus.mockReset();

    mockStatus([
      statusView({
        component: "ffmpeg",
        state: "update_available",
        activeVersion: "7.1",
        availableVersion: "7.2",
        installedVersions: ["7.1"],
        rollbackVersion: null,
      }),
    ]);
    const second = renderSection();
    const update = await screen.findByRole("button", {
      name: en["kernel.update"],
    });
    expect(
      screen.queryByRole("button", { name: en["kernel.rollback"] }),
    ).toBeNull();
    await user.click(update);
    expect(updateComponent).toHaveBeenCalledTimes(1);
    expect(updateComponent).toHaveBeenCalledWith("ffmpeg");
    await waitFor(() => expect(getKernelStatus).toHaveBeenCalledTimes(2));

    const refreshCalls = getKernelStatus.mock.calls.length;
    await user.click(
      screen.getByRole("button", { name: en["kernel.refresh"] }),
    );
    await waitFor(() =>
      expect(getKernelStatus.mock.calls.length).toBeGreaterThan(refreshCalls),
    );
    second.unmount();
    getKernelStatus.mockReset();

    mockStatus([
      statusView({
        component: "deno",
        state: "manifest_unavailable",
        activeVersion: null,
        availableVersion: null,
        installedVersions: [],
        rollbackVersion: null,
      }),
    ]);
    renderSection();
    await screen.findByText("deno");
    const text = document.body.textContent ?? "";
    expect(text).toContain(en["kernel.hint.manifestUnavailable"]);
    const blockedInstall = screen.queryByRole("button", {
      name: en["kernel.install"],
    });
    if (blockedInstall !== null) {
      expect(blockedInstall).toBeDisabled();
    }
  });

  it("polling_runs_only_while_an_operation_is_active", async () => {
    vi.useFakeTimers();
    try {
      let calls = 0;
      getKernelStatus.mockImplementation(async () => {
        calls += 1;
        const active = calls < 4;
        return {
          ok: true,
          data: [
            statusView({
              component: "yt-dlp",
              state: active ? "update_available" : "installed",
              operation: active ? downloadingOperation(10 * calls) : null,
            }),
          ],
        };
      });

      renderSection();
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
      expect(getKernelStatus).toHaveBeenCalledTimes(1);

      for (const expected of [2, 3, 4]) {
        await act(async () => {
          await vi.advanceTimersByTimeAsync(1000);
        });
        expect(getKernelStatus).toHaveBeenCalledTimes(expected);
      }

      await act(async () => {
        await vi.advanceTimersByTimeAsync(5000);
      });
      expect(getKernelStatus).toHaveBeenCalledTimes(4);
    } finally {
      vi.useRealTimers();
    }
  });

  it("typed_errors_are_localized", async () => {
    mockStatus([
      statusView({
        component: "yt-dlp",
        state: "tampered",
        error: { code: "COMPONENT_HASH", message: "raw hash mismatch detail" },
      }),
      statusView({
        component: "ffmpeg",
        state: "tampered",
        error: {
          code: "SOME_FUTURE_CODE",
          message: "raw future failure detail",
        },
      }),
    ]);
    renderSection();

    await screen.findByText("ffmpeg");
    const text = document.body.textContent ?? "";
    expect(text).toContain(en["kernel.error.COMPONENT_HASH"]);
    expect(text).toContain(en["kernel.hint.tampered"]);
    expect(text).toContain(
      translate("en-US", "kernel.error.unknown", { code: "SOME_FUTURE_CODE" }),
    );
    expect(text).toContain("SOME_FUTURE_CODE");
    expect(text).not.toContain("raw hash mismatch detail");
    expect(text).not.toContain("raw future failure detail");
    expect(text).not.toMatch(/\{\s*"code"/);
  });
});

describe("MissingComponentAction", () => {
  it("missing_component_action_opens_components", async () => {
    const enMissingBody = en["kernel.missingBody"];
    const zhMissingBody = zh["kernel.missingBody"];
    expect(enMissingBody).toEqual(expect.any(String));
    expect(zhMissingBody).toEqual(expect.any(String));
    expect(en["kernel.missingTitle"]).toEqual(expect.any(String));
    expect(zh["kernel.missingTitle"]).toEqual(expect.any(String));

    const user = userEvent.setup();
    const onOpenComponents = vi.fn();
    const first = render(
      <I18nProvider language="en-US">
        <MissingComponentAction onOpenComponents={onOpenComponents} />
      </I18nProvider>,
    );

    const button = screen.getByRole("button", {
      name: en["kernel.openComponents"],
    });
    expect(document.body.textContent).toContain(enMissingBody);
    await user.click(button);
    expect(onOpenComponents).toHaveBeenCalledTimes(1);
    first.unmount();

    render(
      <I18nProvider language="zh-CN">
        <MissingComponentAction onOpenComponents={vi.fn()} />
      </I18nProvider>,
    );
    expect(
      screen.getByRole("button", { name: zh["kernel.openComponents"] }),
    ).toBeInTheDocument();
    expect(document.body.textContent).toContain(zhMissingBody);
  });
});

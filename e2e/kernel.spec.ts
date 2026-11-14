import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import en from "../src/i18n/en-US";

import { installMockIpc } from "./support/mock-ipc";

interface MockCall {
  command: string;
  args: Record<string, unknown>;
}

async function mockCalls(page: Page): Promise<MockCall[]> {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __oneGetMock: { calls: MockCall[] };
        }
      ).__oneGetMock.calls,
  );
}

function stateBadge(row: Locator): Locator {
  return row.locator(".og-badge");
}

function versionValue(row: Locator, label: string): Locator {
  return row
    .locator("dt")
    .filter({ hasText: label })
    .locator("xpath=following-sibling::dd");
}

test.describe("components settings with deterministic mock IPC", () => {
  test("installs, updates, and rolls back with state-aware controls", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();

    const settings = page.getByRole("dialog", { name: en["settings.title"] });
    await expect(settings).toBeVisible();
    const components = settings.getByRole("region", {
      name: en["settings.components"],
    });
    await expect(components).toBeVisible();
    await expect(components.getByRole("listitem")).toHaveCount(3);

    const ytDlp = components
      .getByRole("listitem")
      .filter({ hasText: "yt-dlp" });
    const ffmpeg = components
      .getByRole("listitem")
      .filter({ hasText: "ffmpeg" });
    const deno = components.getByRole("listitem").filter({ hasText: "deno" });

    await expect(stateBadge(ytDlp)).toHaveText(
      en["kernel.state.not_installed"],
    );
    await expect(stateBadge(ffmpeg)).toHaveText(
      en["kernel.state.update_available"],
    );
    await expect(stateBadge(deno)).toHaveText(en["kernel.state.installed"]);

    await expect(
      ffmpeg.getByRole("button", { name: en["kernel.update"] }),
    ).toBeVisible();
    await expect(
      ffmpeg.getByRole("button", { name: en["kernel.rollback"] }),
    ).toHaveCount(0);
    await expect(
      deno.getByRole("button", { name: en["kernel.rollback"] }),
    ).toBeVisible();
    await expect(
      deno.getByRole("button", { name: en["kernel.update"] }),
    ).toHaveCount(0);

    await ytDlp.getByRole("button", { name: en["kernel.install"] }).click();
    await expect(stateBadge(ytDlp)).toHaveText(en["kernel.state.installed"]);
    await expect(versionValue(ytDlp, en["kernel.activeVersion"])).toHaveText(
      "2025.11.01",
    );
    await expect(
      ytDlp.getByRole("button", { name: en["kernel.install"] }),
    ).toHaveCount(0);

    await ffmpeg.getByRole("button", { name: en["kernel.update"] }).click();
    await expect(stateBadge(ffmpeg)).toHaveText(en["kernel.state.installed"]);
    await expect(versionValue(ffmpeg, en["kernel.activeVersion"])).toHaveText(
      "7.2",
    );
    await expect(
      ffmpeg.getByRole("button", { name: en["kernel.update"] }),
    ).toHaveCount(0);
    await expect(
      ffmpeg.getByRole("button", { name: en["kernel.rollback"] }),
    ).toBeVisible();

    await deno.getByRole("button", { name: en["kernel.rollback"] }).click();
    await expect(stateBadge(deno)).toHaveText(en["kernel.state.installed"]);
    await expect(versionValue(deno, en["kernel.activeVersion"])).toHaveText(
      "2.4.0",
    );
    await expect(
      deno.getByRole("button", { name: en["kernel.rollback"] }),
    ).toHaveCount(0);
    await expect(deno.getByText(en["kernel.rollbackTo"])).toHaveCount(0);

    const calls = await mockCalls(page);
    expect(calls.filter((call) => call.command === "kernel_install")).toEqual([
      { command: "kernel_install", args: { component: "yt-dlp" } },
    ]);
    expect(calls.filter((call) => call.command === "kernel_update")).toEqual([
      { command: "kernel_update", args: { component: "ffmpeg" } },
    ]);
    expect(calls.filter((call) => call.command === "kernel_rollback")).toEqual([
      { command: "kernel_rollback", args: { component: "deno" } },
    ]);
  });
});

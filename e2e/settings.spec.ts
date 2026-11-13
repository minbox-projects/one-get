import { expect, test } from "@playwright/test";

import en from "../src/i18n/en-US";
import zh from "../src/i18n/zh-CN";

import { installMockIpc } from "./support/mock-ipc";

test.describe("settings with deterministic mock IPC", () => {
  test("switches language and theme immediately, then persists them across reopen", async ({
    page,
  }) => {
    await installMockIpc(page, {
      settings: { language: "en-US", theme: "system" },
    });
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();

    const dialog = page.getByRole("dialog", { name: en["settings.title"] });
    await expect(dialog.getByLabel(en["settings.language"])).toHaveValue(
      "en-US",
    );

    await dialog
      .getByLabel(en["settings.language"])
      .selectOption("zh-CN");
    await expect(
      page.getByRole("button", { name: zh["toolbar.new"] }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: zh["toolbar.settings"] }),
    ).toBeVisible();

    await dialog.getByLabel(zh["settings.theme"]).selectOption("dark");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

    await dialog.getByRole("button", { name: zh["settings.save"] }).click();
    await expect(dialog).not.toBeVisible();

    await page.getByRole("button", { name: zh["toolbar.settings"] }).click();
    const reopened = page.getByRole("dialog", { name: zh["settings.title"] });
    await expect(reopened.getByLabel(zh["settings.language"])).toHaveValue(
      "zh-CN",
    );
    await expect(reopened.getByLabel(zh["settings.theme"])).toHaveValue("dark");
  });

  test("shows the read-only message and disables saving for a future schema", async ({
    page,
  }) => {
    await installMockIpc(page, {
      settings: { schemaVersion: 3 },
      status: { readOnly: true, recovered: false },
    });
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();

    const dialog = page.getByRole("dialog", { name: en["settings.title"] });
    await expect(dialog.getByRole("alert")).toContainText(
      en["settings.readOnly"],
    );
    await expect(
      dialog.getByRole("button", { name: en["settings.save"] }),
    ).toBeDisabled();
  });
});

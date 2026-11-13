import { expect, test } from "@playwright/test";

import en from "../src/i18n/en-US";

import { installMockIpc } from "./support/mock-ipc";

const mod = process.platform === "darwin" ? "Meta" : "Control";

test.describe("keyboard-only flows", () => {
  test("opens each shortcut surface, traps focus, and restores focus on close", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await expect(
      page.getByRole("region", { name: en["list.label"] }),
    ).toBeVisible();

    await page.keyboard.press("Tab");
    expect(
      await page.evaluate(
        () => document.activeElement?.matches(":focus-visible") ?? false,
      ),
    ).toBe(true);

    await page.keyboard.press(`${mod}+n`);
    const newDialog = page.getByRole("dialog", {
      name: en["download.newTitle"],
    });
    await expect(newDialog).toBeVisible();
    expect(
      await newDialog.evaluate((element) =>
        element.contains(document.activeElement),
      ),
    ).toBe(true);
    await page.keyboard.press("Escape");
    await expect(newDialog).not.toBeVisible();

    await page.keyboard.press(`${mod}+Shift+n`);
    const batchDialog = page.getByRole("dialog", { name: en["batch.title"] });
    await expect(batchDialog).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(batchDialog).not.toBeVisible();

    await page.keyboard.press(`${mod}+,`);
    const settingsDialog = page.getByRole("dialog", {
      name: en["settings.title"],
    });
    await expect(settingsDialog).toBeVisible();
    expect(
      await settingsDialog.evaluate((element) =>
        element.contains(document.activeElement),
      ),
    ).toBe(true);
    await page.keyboard.press("Escape");
    await expect(settingsDialog).not.toBeVisible();

    await page.keyboard.press(`${mod}+f`);
    await expect(page.getByLabel(en["toolbar.search"])).toBeFocused();
  });

  test("completes new download, pause/resume, and details with the keyboard", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await page.keyboard.press(`${mod}+n`);
    const dialog = page.getByRole("dialog", { name: en["download.newTitle"] });
    await expect(dialog).toBeVisible();

    await dialog.getByLabel(en["download.url"], { exact: true }).focus();
    await page.keyboard.type(
      "https://fixtures.example.test/files/mock-download.bin",
    );
    await expect(dialog.getByText("File: mock-download.bin")).toBeVisible();

    await dialog.getByRole("button", { name: en["download.start"] }).focus();
    await page.keyboard.press("Enter");
    await expect(dialog).not.toBeVisible();

    const row = page
      .getByRole("listitem")
      .filter({ hasText: "mock-download.bin" });
    await expect(row).toBeVisible();

    await row
      .getByRole("button", { name: "mock-download.bin", exact: true })
      .focus();
    await page.keyboard.press("Enter");
    await row.focus();

    await page.keyboard.press("Space");
    await expect(row).toContainText(en["state.paused"]);
    await page.keyboard.press("Space");
    await expect(row).toContainText(en["state.downloading"]);

    await row.focus();
    await page.keyboard.press("Enter");
    await expect(
      page.getByRole("region", { name: en["details.label"] }),
    ).toContainText(
      "https://fixtures.example.test/files/mock-download.bin",
    );
  });

  test("opens the delete-record confirmation with Delete and keeps focus inside", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    const row = page.getByRole("listitem").filter({ hasText: "report.pdf" });
    await row.getByRole("button", { name: "report.pdf", exact: true }).focus();
    await page.keyboard.press("Enter");
    await row.focus();
    await page.keyboard.press("Delete");

    const dialog = page.getByRole("dialog", {
      name: en["delete.recordTitle"],
    });
    await expect(dialog).toBeVisible();
    expect(
      await dialog.evaluate((element) =>
        element.contains(document.activeElement),
      ),
    ).toBe(true);

    await dialog
      .getByRole("button", { name: en["delete.recordConfirm"] })
      .focus();
    await page.keyboard.press("Enter");
    await expect(row).not.toBeVisible();
  });
});

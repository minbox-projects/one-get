import { expect, test } from "@playwright/test";

test.describe("design system in the task-first shell", () => {
  test("keeps labelled dialog controls, an accessible close action, and focus restoration", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") {
        consoleErrors.push(message.text());
      }
    });

    await page.goto("/");

    const trigger = page.getByRole("button", { name: "New download" });
    await trigger.click();

    const dialog = page.getByRole("dialog", { name: "New download" });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel("URL")).toBeVisible();
    await expect(dialog.getByLabel("Connections")).toBeVisible();
    await expect(
      dialog.getByRole("button", { name: "Close" }),
    ).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(dialog).not.toBeVisible();
    await expect(trigger).toBeFocused();
    expect(consoleErrors).toEqual([]);
  });

  test("keeps the shell heading and list visible in dark mode", async ({
    page,
  }) => {
    await page.emulateMedia({ colorScheme: "dark" });
    await page.goto("/");

    await expect(
      page.getByRole("heading", { level: 1, name: "OneGet" }),
    ).toBeVisible();
    await expect(
      page.getByRole("region", { name: "Download list" }),
    ).toBeVisible();
  });
});

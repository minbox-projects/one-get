import { expect, test } from "@playwright/test";

test.describe("design system preview", () => {
  test("opens the dialog from the preview, shows its title, and restores focus without console errors", async ({
    page,
  }) => {
    const consoleErrors: string[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") {
        consoleErrors.push(message.text());
      }
    });

    await page.goto("/");

    await expect(
      page.getByRole("heading", { name: "Design system" }),
    ).toBeVisible();

    const trigger = page.getByRole("button", { name: "Open dialog" });
    await trigger.click();

    await expect(
      page.getByRole("dialog", { name: "Design system dialog" }),
    ).toBeVisible();

    await page.keyboard.press("Escape");

    await expect(
      page.getByRole("dialog", { name: "Design system dialog" }),
    ).not.toBeVisible();
    await expect(trigger).toBeFocused();
    expect(consoleErrors).toEqual([]);
  });

  test("keeps the shell heading visible in dark mode", async ({ page }) => {
    await page.emulateMedia({ colorScheme: "dark" });
    await page.goto("/");

    await expect(page.getByRole("heading", { name: "OneGet" })).toBeVisible();
  });
});

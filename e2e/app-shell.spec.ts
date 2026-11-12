import { expect, test } from "@playwright/test";

test.describe("app shell browser preview", () => {
  test("shows OneGet heading, diagnostics surface, runtime-unavailable banner, and keeps dialog usable without console errors", async ({
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
      page.getByRole("heading", { level: 1, name: "OneGet" }),
    ).toBeVisible();
    await expect(
      page.getByRole("region", { name: /diagnostics/i }),
    ).toBeVisible();
    await expect(page.getByText(/runtime unavailable/i)).toBeVisible();

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
});

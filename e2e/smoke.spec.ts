import { expect, test } from "@playwright/test";

test("shows the OneGet shell", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1, name: "OneGet" })).toBeVisible();
  await expect(page).toHaveTitle(/OneGet/);
});

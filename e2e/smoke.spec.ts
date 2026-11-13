import { expect, test } from "@playwright/test";

test("shows the task-first OneGet shell", async ({ page }) => {
  await page.goto("/");

  await expect(
    page.getByRole("heading", { level: 1, name: "OneGet" }),
  ).toBeVisible();
  await expect(page).toHaveTitle(/OneGet/);
  await expect(page.getByRole("navigation", { name: "Library" })).toBeVisible();
  await expect(
    page.getByRole("region", { name: "Download list" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "New download" }),
  ).toBeVisible();
});

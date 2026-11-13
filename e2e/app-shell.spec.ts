import { expect, test } from "@playwright/test";

test.describe("app shell browser preview", () => {
  test("renders the task-first shell with a runtime-unavailable fallback and no console errors", async ({
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
      page.getByRole("navigation", { name: "Library" }),
    ).toBeVisible();
    for (const name of [
      "All downloads",
      "Downloading",
      "Completed",
      "Failed",
      "Video",
      "Model",
      "Document",
      "Program",
      "Other",
    ]) {
      await expect(
        page.getByRole("button", { name: new RegExp(name) }),
      ).toBeVisible();
    }
    for (const name of [
      "New download",
      "Batch add",
      "Pause all",
      "Resume all",
      "Settings",
    ]) {
      await expect(page.getByRole("button", { name })).toBeVisible();
    }
    await expect(page.getByLabel("Search downloads")).toBeVisible();
    await expect(
      page.getByRole("region", { name: "Download list" }),
    ).toBeVisible();
    await expect(page.getByText(/runtime.*unavailable/i)).toBeVisible();

    const trigger = page.getByRole("button", { name: "New download" });
    await trigger.click();

    const dialog = page.getByRole("dialog", { name: "New download" });
    await expect(dialog).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(dialog).not.toBeVisible();
    await expect(trigger).toBeFocused();
    expect(consoleErrors).toEqual([]);
  });
});

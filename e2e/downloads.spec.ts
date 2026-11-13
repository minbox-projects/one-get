import { expect, test } from "@playwright/test";

import en from "../src/i18n/en-US";

import { installMockIpc } from "./support/mock-ipc";

const listRegion = (page: Parameters<typeof installMockIpc>[0]) =>
  page.getByRole("region", { name: en["list.label"] });

test.describe("downloads with deterministic mock IPC", () => {
  test("adds a download through the probe without public network access", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await expect(listRegion(page)).toBeVisible();
    await page.getByRole("button", { name: en["toolbar.new"] }).click();

    const dialog = page.getByRole("dialog", { name: en["download.newTitle"] });
    await dialog
      .getByLabel(en["download.url"], { exact: true })
      .fill("https://fixtures.example.test/files/mock-download.bin");

    await expect(
      dialog.getByText(`File: mock-download.bin`),
    ).toBeVisible();
    await expect(
      dialog.getByText(en["download.probeRange"]),
    ).toBeVisible();

    await dialog
      .getByLabel(en["download.destination"], { exact: true })
      .fill("/tmp/oneget");
    await dialog
      .getByRole("button", { name: en["download.start"] })
      .click();

    await expect(dialog).not.toBeVisible();
    const row = page
      .getByRole("listitem")
      .filter({ hasText: "mock-download.bin" });
    await expect(row).toBeVisible();
    await expect(row).toContainText(en["state.downloading"]);
  });

  test("pauses and resumes a selected task", async ({ page }) => {
    await installMockIpc(page);
    await page.goto("/");

    const row = page
      .getByRole("listitem")
      .filter({ hasText: "alpha-video.mp4" });
    await expect(row).toBeVisible();

    await row
      .getByRole("button", { name: en["task.pause"], exact: true })
      .click();
    await expect(row).toContainText(en["state.paused"]);

    await row
      .getByRole("button", { name: en["task.resume"], exact: true })
      .click();
    await expect(row).toContainText(en["state.downloading"]);
  });

  test("cancels a task without creating a final file", async ({ page }) => {
    await installMockIpc(page);
    await page.goto("/");

    const row = page
      .getByRole("listitem")
      .filter({ hasText: "alpha-video.mp4" });
    await expect(row).toBeVisible();

    await row
      .getByRole("button", { name: en["task.cancel"], exact: true })
      .click();
    await expect(row).toContainText(en["state.cancelled"]);

    const snapshot = await page.evaluate(
      () =>
        (
          window as unknown as {
            __oneGetMock: {
              snapshot: () => { files: string[] };
            };
          }
        ).__oneGetMock.snapshot(),
    );
    expect(snapshot.files).not.toContain("alpha-video.mp4");
  });

  test("deletes a record only after confirmation and keeps the final file", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    const row = page.getByRole("listitem").filter({ hasText: "report.pdf" });
    await row.getByRole("button", { name: "report.pdf", exact: true }).click();

    const details = page.getByRole("region", { name: en["details.label"] });
    await expect(details).toBeVisible();
    await details
      .getByRole("button", { name: en["delete.recordConfirm"] })
      .click();

    const dialog = page.getByRole("dialog", {
      name: en["delete.recordTitle"],
    });
    await expect(dialog).toContainText(en["delete.recordBody"]);
    await dialog
      .getByRole("button", { name: en["delete.recordConfirm"] })
      .click();

    await expect(row).not.toBeVisible();
    const snapshot = await page.evaluate(() => {
      const bridge = (
        window as unknown as {
          __oneGetMock: {
            calls: Array<{ command: string }>;
            snapshot: () => { files: string[] };
          };
        }
      ).__oneGetMock;
      return {
        files: bridge.snapshot().files,
        commands: bridge.calls.map((call) => call.command),
      };
    });
    expect(snapshot.files).toContain("report.pdf");
    expect(snapshot.commands).not.toContain("downloads_delete_file");
  });

  test("requires the file-deletion acknowledgement and removes the file", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    const row = page.getByRole("listitem").filter({ hasText: "report.pdf" });
    await row.getByRole("button", { name: "report.pdf", exact: true }).click();

    const details = page.getByRole("region", { name: en["details.label"] });
    await expect(details).toBeVisible();
    await details
      .getByRole("button", { name: en["delete.fileConfirm"] })
      .click();

    const dialog = page.getByRole("dialog", { name: en["delete.fileTitle"] });
    const confirm = dialog.getByRole("button", {
      name: en["delete.fileConfirm"],
    });
    await expect(confirm).toBeDisabled();

    await dialog.getByRole("checkbox").check();
    await expect(confirm).toBeEnabled();
    await confirm.click();

    await expect(row).not.toBeVisible();
    const snapshot = await page.evaluate(
      () =>
        (
          window as unknown as {
            __oneGetMock: { snapshot: () => { files: string[] } };
          }
        ).__oneGetMock.snapshot(),
    );
    expect(snapshot.files).not.toContain("report.pdf");
  });

  test("scrolls a 10,000-row list to the last task with bounded mounted rows", async ({
    page,
  }) => {
    await installMockIpc(page, { taskCount: 10_000 });
    await page.goto("/");

    const list = page.getByRole("list", { name: en["list.label"] });
    await expect(list).toBeVisible();
    await expect(page.getByText("file-09999.mp4")).toBeVisible();

    const mounted = await page.getByTestId("task-row").count();
    expect(mounted).toBeLessThanOrEqual(100);

    await list.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
      element.dispatchEvent(new Event("scroll", { bubbles: true }));
    });

    await expect(page.getByText("file-00000.mp4")).toBeVisible();
  });
});

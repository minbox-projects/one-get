import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import en from "../src/i18n/en-US";
import type { PendingPrefill } from "../src/types/pairing";

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

async function addCalls(page: Page): Promise<MockCall[]> {
  return (await mockCalls(page)).filter(
    (call) => call.command === "downloads_add",
  );
}

function prefill(
  items: Array<{
    url: string;
    filename: string;
    referer: string;
  }>,
  receivedAtMs: number,
): PendingPrefill {
  return {
    items: items.map((item) => ({
      ...item,
      mime: "application/octet-stream",
      size: 2048,
      headers: [],
    })),
    receivedAtMs,
  };
}

test.describe("capture prefill delivery with deterministic mock IPC", () => {
  test("opens New Download with captured metadata, probes, and adds one task on submit", async ({
    page,
  }) => {
    const referer = "https://page.example.test/watch";
    const url = "https://fixtures.example.test/media/stream";
    await installMockIpc(page, {
      pendingPrefills: [
        prefill([{ url, filename: "captured-video.mp4", referer }], 1),
      ],
    });
    await page.goto("/");

    const dialog = page.getByRole("dialog", {
      name: en["download.newTitle"],
    });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel(en["download.url"], { exact: true })).toHaveValue(
      url,
    );

    await expect(
      dialog.getByText(
        en["download.probeFilename"].replace("{filename}", "stream"),
      ),
    ).toBeVisible();
    await expect(
      dialog.getByLabel(en["download.filename"], { exact: true }),
    ).toHaveValue("captured-video.mp4");
    await expect(dialog.getByText(en["download.browserPrefill"])).toBeVisible();

    await dialog.getByText(en["download.advancedHeaders"]).click();
    await expect(
      dialog.getByLabel(en["download.headerName"], { exact: true }),
    ).toHaveValue("Referer");
    await expect(
      dialog.getByLabel(en["download.headerValue"], { exact: true }),
    ).toHaveValue(referer);

    expect(await addCalls(page)).toHaveLength(0);

    await dialog.getByRole("button", { name: en["download.start"] }).click();
    await expect(dialog).not.toBeVisible();
    await expect.poll(async () => (await addCalls(page)).length).toBe(1);

    const request = (await addCalls(page))[0].args.request as {
      url?: string;
      filename?: string;
      extraHeaders?: string[][];
    };
    expect(request).toMatchObject({
      url,
      filename: "captured-video.mp4",
      extraHeaders: [["Referer", referer]],
    });
  });

  test("opens Batch Download with captured URLs and adds one task per URL on submit", async ({
    page,
  }) => {
    const items = [
      {
        url: "https://fixtures.example.test/files/captured-a.bin",
        filename: "captured-a.bin",
        referer: "https://page.example.test/a",
      },
      {
        url: "https://fixtures.example.test/files/captured-b.bin",
        filename: "captured-b.bin",
        referer: "https://page.example.test/b",
      },
    ];
    await installMockIpc(page, {
      pendingPrefills: [prefill(items, 2)],
    });
    await page.goto("/");

    const dialog = page.getByRole("dialog", { name: en["batch.title"] });
    await expect(dialog).toBeVisible();
    await expect(dialog.getByLabel(en["batch.urls"])).toHaveValue(
      items.map((item) => item.url).join("\n"),
    );
    await expect(dialog.getByText(en["batch.browserPrefill"])).toBeVisible();
    expect(await addCalls(page)).toHaveLength(0);

    await dialog.getByRole("button", { name: en["batch.start"] }).click();
    await expect.poll(async () => (await addCalls(page)).length).toBe(2);

    const requests = (await addCalls(page)).map(
      (call) =>
        call.args.request as {
          url?: string;
          extraHeaders?: string[][];
        },
    );
    expect(requests.map((request) => request.url).sort()).toEqual(
      items.map((item) => item.url).sort(),
    );
    for (const request of requests) {
      expect(request.extraHeaders).toEqual([]);
    }
  });
});

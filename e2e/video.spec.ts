import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import en from "../src/i18n/en-US";
import { formatBytes, formatEta } from "../src/lib/format";
import type { DownloadSummary } from "../src/types/downloads";
import type { VideoStartRequest } from "../src/types/video";

import { installMockIpc } from "./support/mock-ipc";

interface MockCall {
  command: string;
  args: Record<string, unknown>;
}

interface MockSnapshot {
  tasks: Array<{
    id: string;
    state: string;
    kind?: string;
    filename?: string;
  }>;
  files: string[];
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

async function mockSnapshot(page: Page): Promise<MockSnapshot> {
  return page.evaluate(
    () =>
      (
        window as unknown as {
          __oneGetMock: { snapshot: () => MockSnapshot };
        }
      ).__oneGetMock.snapshot(),
  );
}

const seededVideoTask: DownloadSummary = {
  id: "video-seeded",
  kind: "video",
  video: {
    title: "Seeded Lecture",
    durationSecs: 754,
    uploader: "Fixture Channel",
    quality: "1080p",
    stage: null,
    audioOnly: false,
  },
  url: "https://fixtures.example.test/watch/seeded-lecture",
  filename: "Seeded Lecture.mp4",
  destination: "/tmp/oneget",
  state: "completed",
  downloadedBytes: 48 * 1024 * 1024,
  totalBytes: 48 * 1024 * 1024,
  connections: 1,
  createdAtMs: 1_700_000_500_000,
  updatedAtMs: 1_700_000_600_000,
  completedAtMs: 1_700_000_600_000,
  errorCode: null,
  errorMessage: null,
};

test.describe("video downloads with deterministic mock IPC", () => {
  test("parses a video, starts exactly one audio task, and pauses it", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.new"] }).click();
    const form = page.getByRole("dialog", { name: en["download.newTitle"] });
    await form
      .getByLabel(en["download.url"], { exact: true })
      .fill("https://fixtures.example.test/watch/fixture-tutorial");
    await form
      .getByRole("button", { name: en["download.parseVideo"] })
      .click();

    const quality = page.getByRole("dialog", {
      name: en["video.qualityTitle"],
    });
    await expect(quality).toBeVisible();
    await expect(quality).toContainText("Fixture Tutorial");
    await expect(quality).toContainText("Fixture Studio");
    await expect(quality).toContainText(
      `${en["video.duration"]}: ${formatEta(615, "en-US") ?? "unavailable"}`,
    );
    await expect(quality.locator("img")).toHaveCount(0);

    const defaultVideo = quality.getByRole("radio", { name: /1080p/ });
    await expect(defaultVideo).toBeChecked();
    const videoChoice = quality.locator("label").filter({ hasText: "1080p" });
    await expect(videoChoice).toContainText(
      formatBytes(42 * 1024 * 1024, "en-US") ?? "unavailable",
    );
    await expect(videoChoice).toContainText(en["video.mp4"]);

    const audioChoice = quality.getByRole("radio", {
      name: /Audio 128 kbps/,
    });
    await expect(audioChoice).toBeVisible();
    const audioLabel = quality
      .locator("label")
      .filter({ hasText: "Audio 128 kbps" });
    await expect(audioLabel).toContainText(
      formatBytes(9 * 1024 * 1024, "en-US") ?? "unavailable",
    );
    await audioChoice.check();

    await quality.getByRole("button", { name: en["video.start"] }).click();

    await expect(quality).not.toBeVisible();
    await expect(form).not.toBeVisible();

    const calls = await mockCalls(page);
    const startCalls = calls.filter((call) => call.command === "video_start");
    expect(startCalls).toHaveLength(1);
    expect(calls.some((call) => call.command === "downloads_add")).toBe(false);
    const request = startCalls[0].args.request as VideoStartRequest;
    expect(request).toMatchObject({
      url: "https://fixtures.example.test/watch/fixture-tutorial",
      filename: "Fixture Tutorial.mp4",
      audioOnly: true,
      formatSelector: "bestaudio/best",
      metadata: { title: "Fixture Tutorial", quality: "Audio 128 kbps" },
    });

    const row = page
      .getByRole("listitem")
      .filter({ hasText: "Fixture Tutorial.mp4" });
    await expect(row).toHaveCount(1);
    await expect(row).toContainText(en["state.downloading"]);
    await expect(row).toContainText(en["nav.video"]);

    const snapshot = await mockSnapshot(page);
    expect(snapshot.tasks.filter((task) => task.kind === "video")).toHaveLength(
      1,
    );

    await row
      .getByRole("button", { name: en["task.pause"], exact: true })
      .click();
    await expect(row).toContainText(en["state.paused"]);
  });

  test("offers the Components entry when a probe reports a missing component", async ({
    page,
  }) => {
    await installMockIpc(page, {
      videoProbeError: {
        code: "COMPONENT_MISSING",
        message: "ffprobe is missing.",
      },
    });
    await page.goto("/");
    await expect(
      page.getByRole("region", { name: en["list.label"] }),
    ).toBeVisible();
    const before = await mockSnapshot(page);

    await page.getByRole("button", { name: en["toolbar.new"] }).click();
    const form = page.getByRole("dialog", { name: en["download.newTitle"] });
    await form
      .getByLabel(en["download.url"], { exact: true })
      .fill("https://fixtures.example.test/watch/needs-components");
    await form
      .getByRole("button", { name: en["download.parseVideo"] })
      .click();

    await expect(form.getByText(en["error.COMPONENT_MISSING"])).toBeVisible();
    await expect(form.getByText(en["kernel.missingTitle"])).toBeVisible();
    await expect(form.getByText(en["kernel.missingBody"])).toBeVisible();
    await expect(
      page.getByRole("dialog", { name: en["video.qualityTitle"] }),
    ).toHaveCount(0);

    const afterProbeCalls = await mockCalls(page);
    expect(
      afterProbeCalls.filter((call) => call.command === "video_start"),
    ).toHaveLength(0);
    expect((await mockSnapshot(page)).tasks).toHaveLength(before.tasks.length);

    await form
      .getByRole("button", { name: en["kernel.openComponents"] })
      .click();

    const settings = page.getByRole("dialog", { name: en["settings.title"] });
    await expect(settings).toBeVisible();
    await expect(form).not.toBeVisible();
    const components = settings.getByRole("region", {
      name: en["settings.components"],
    });
    await expect(components).toBeVisible();
    await expect(components).toContainText("yt-dlp");

    const afterOpenCalls = await mockCalls(page);
    expect(
      afterOpenCalls.filter((call) => call.command === "video_start"),
    ).toHaveLength(0);
    expect((await mockSnapshot(page)).tasks).toHaveLength(before.tasks.length);
  });

  test("shows a seeded video task with metadata and no byte segments", async ({
    page,
  }) => {
    await installMockIpc(page, { videoTasks: [seededVideoTask] });
    await page.goto("/");

    const row = page
      .getByRole("listitem")
      .filter({ hasText: "Seeded Lecture.mp4" });
    await expect(row).toBeVisible();
    await expect(row).toContainText(en["nav.video"]);

    await row
      .getByRole("button", { name: "Seeded Lecture.mp4", exact: true })
      .click();

    const details = page.getByRole("region", { name: en["details.label"] });
    const video = details.getByRole("region", { name: en["details.video"] });
    await expect(video).toBeVisible();
    await expect(video).toContainText("Seeded Lecture");
    await expect(video).toContainText("Fixture Channel");
    await expect(video).toContainText("1080p");
    await expect(video).toContainText(
      formatEta(754, "en-US") ?? "unavailable",
    );
    await expect(
      details.getByRole("region", { name: en["details.segments"] }),
    ).toHaveCount(0);
  });
});

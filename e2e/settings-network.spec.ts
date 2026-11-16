import { expect, test } from "@playwright/test";

import en from "../src/i18n/en-US";

import { installMockIpc } from "./support/mock-ipc";

interface SavedSettings {
  defaultCookiesFile?: string | null;
  userAgent?: string | null;
  hfConnectionCap?: number;
  alwaysAskQuality?: boolean;
  videoQualityPreference?: string;
}

test.describe("network, Hugging Face, and video settings", () => {
  test("edits and persists the network, Hugging Face, and video sections", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();
    const dialog = page.getByRole("dialog", { name: en["settings.title"] });

    await dialog
      .getByLabel(en["settings.network.defaultCookiesFile"])
      .fill("/tmp/cookies.txt");
    await dialog
      .getByLabel(en["settings.network.userAgent"])
      .fill("OneGetAgent/1.0");
    await dialog
      .getByLabel(en["settings.huggingFace.connectionCap"])
      .fill("4");
    await dialog.getByLabel(en["settings.video.alwaysAsk"]).uncheck();
    await dialog
      .getByLabel(en["settings.video.qualityPreference"])
      .selectOption("highest");

    await dialog.getByRole("button", { name: en["settings.save"] }).click();
    await expect(dialog).not.toBeVisible();

    const saved = await page.evaluate(() => {
      const calls = (
        window as unknown as {
          __oneGetMock: {
            calls: Array<{ command: string; args: Record<string, unknown> }>;
          };
        }
      ).__oneGetMock.calls;
      const call = calls.find((entry) => entry.command === "settings_save");
      return call?.args.settings as SavedSettings | undefined;
    });

    expect(saved?.defaultCookiesFile).toBe("/tmp/cookies.txt");
    expect(saved?.userAgent).toBe("OneGetAgent/1.0");
    expect(saved?.hfConnectionCap).toBe(4);
    expect(saved?.alwaysAskQuality).toBe(false);
    expect(saved?.videoQualityPreference).toBe("highest");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();
    const reopened = page.getByRole("dialog", {
      name: en["settings.title"],
    });
    await expect(
      reopened.getByLabel(en["settings.network.defaultCookiesFile"]),
    ).toHaveValue("/tmp/cookies.txt");
    await expect(
      reopened.getByLabel(en["settings.network.userAgent"]),
    ).toHaveValue("OneGetAgent/1.0");
    await expect(
      reopened.getByLabel(en["settings.video.alwaysAsk"]),
    ).not.toBeChecked();
  });

  test("keeps the Hugging Face token write-only and reports its status", async ({
    page,
  }) => {
    await installMockIpc(page);
    await page.goto("/");

    await page.getByRole("button", { name: en["toolbar.settings"] }).click();
    const dialog = page.getByRole("dialog", { name: en["settings.title"] });

    await expect(
      dialog.getByText(en["settings.huggingFace.tokenNotConfigured"]),
    ).toBeVisible();
    const token = dialog.getByLabel(en["settings.huggingFace.token"]);
    await expect(token).toHaveAttribute("type", "password");

    const save = dialog.getByRole("button", {
      name: en["settings.huggingFace.saveToken"],
    });
    await expect(save).toBeDisabled();
    await token.fill("hf_fixture_token");
    await expect(save).toBeEnabled();
    await save.click();

    await expect(
      dialog.getByText(en["settings.huggingFace.tokenConfigured"]),
    ).toBeVisible();
    await expect(token).toHaveValue("");
    await expect(
      dialog.getByText("hf_fixture_token"),
    ).not.toBeVisible();
  });
});

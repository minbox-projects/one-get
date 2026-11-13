import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import {
  dictionaries,
  I18nProvider,
  resolveLanguage,
  translate,
  useI18n,
} from "./index";

const REQUIRED_KEYS = [
  "app.title",
  "nav.label",
  "nav.all",
  "nav.downloading",
  "nav.completed",
  "nav.failed",
  "nav.video",
  "nav.model",
  "nav.document",
  "nav.program",
  "nav.other",
  "toolbar.new",
  "toolbar.batch",
  "toolbar.pauseAll",
  "toolbar.resumeAll",
  "toolbar.search",
  "toolbar.settings",
  "list.label",
  "list.emptyTitle",
  "list.emptyBody",
  "details.label",
  "details.empty",
  "status.active",
  "status.speed",
  "common.unknown",
  "common.loading",
  "common.retry",
  "error.runtimeUnavailable",
];

function Formats({
  value,
  date,
}: {
  value: number | null;
  date?: string | null;
}) {
  const { formatBytes, formatSpeed, formatEta, formatDate, formatNumber } =
    useI18n();

  return (
    <div>
      <output data-testid="bytes">{formatBytes(value) ?? "null"}</output>
      <output data-testid="speed">{formatSpeed(value) ?? "null"}</output>
      <output data-testid="eta">{formatEta(value) ?? "null"}</output>
      <output data-testid="date">{formatDate(date) ?? "null"}</output>
      <output data-testid="number">{formatNumber(1234.5)}</output>
    </div>
  );
}

function LanguageSwitch() {
  const { t, setLanguage } = useI18n();

  return (
    <div>
      <span data-testid="label">{t("nav.all")}</span>
      <button type="button" onClick={() => setLanguage("zh-CN")}>
        switch language
      </button>
    </div>
  );
}

describe("i18n", () => {
  it("keeps zh-CN and en-US dictionaries in key parity with every required key", () => {
    const zhKeys = Object.keys(dictionaries["zh-CN"]).sort();
    const enKeys = Object.keys(dictionaries["en-US"]).sort();

    expect(zhKeys).toEqual(enKeys);
    for (const key of REQUIRED_KEYS) {
      expect(dictionaries["zh-CN"][key]).toEqual(expect.any(String));
      expect(dictionaries["en-US"][key]).toEqual(expect.any(String));
      expect(dictionaries["zh-CN"][key].length).toBeGreaterThan(0);
      expect(dictionaries["en-US"][key].length).toBeGreaterThan(0);
    }
  });

  it("interpolates message parameters", () => {
    const messages = dictionaries["en-US"] as Record<string, string>;
    const previous = messages["test.interpolation"];
    messages["test.interpolation"] = "Hello {name}, {count} files";

    try {
      expect(
        translate("en-US", "test.interpolation" as never, {
          name: "Ada",
          count: 2,
        }),
      ).toBe("Hello Ada, 2 files");
    } finally {
      if (previous === undefined) {
        delete messages["test.interpolation"];
      } else {
        messages["test.interpolation"] = previous;
      }
    }
  });

  it("resolves explicit and system languages", () => {
    expect(resolveLanguage("zh-CN")).toBe("zh-CN");
    expect(resolveLanguage("en-US")).toBe("en-US");
    expect(resolveLanguage("system", "zh-CN")).toBe("zh-CN");
    expect(resolveLanguage("system", "zh-TW")).toBe("zh-CN");
    expect(resolveLanguage("system", "en-GB")).toBe("en-US");
    expect(resolveLanguage("system", "fr-FR")).toBe("en-US");
  });

  it("returns null for unknown byte, speed, ETA, and date values", () => {
    render(
      <I18nProvider language="en-US">
        <Formats value={null} date={null} />
      </I18nProvider>,
    );

    expect(screen.getByTestId("bytes")).toHaveTextContent("null");
    expect(screen.getByTestId("speed")).toHaveTextContent("null");
    expect(screen.getByTestId("eta")).toHaveTextContent("null");
    expect(screen.getByTestId("date")).toHaveTextContent("null");
  });

  it("formats bytes and dates with locale-aware strings", () => {
    const english = render(
      <I18nProvider language="en-US">
        <Formats value={1536} date="2026-11-13T12:00:00Z" />
      </I18nProvider>,
    );
    const englishBytes = english.getByTestId("bytes").textContent ?? "";
    const englishDate = english.getByTestId("date").textContent ?? "";
    english.unmount();

    const chinese = render(
      <I18nProvider language="zh-CN">
        <Formats value={1536} date="2026-11-13T12:00:00Z" />
      </I18nProvider>,
    );
    const chineseBytes = chinese.getByTestId("bytes").textContent ?? "";
    const chineseDate = chinese.getByTestId("date").textContent ?? "";

    expect(englishBytes).toMatch(/1\.5\s?KB/i);
    expect(chineseBytes).toMatch(/1\.5\s?KB/i);
    expect(englishDate).not.toBe("");
    expect(chineseDate).not.toBe("");
    expect(englishDate).not.toBe(chineseDate);
  });

  it("switches language through the provider hook", async () => {
    const user = userEvent.setup();

    render(
      <I18nProvider language="en-US">
        <LanguageSwitch />
      </I18nProvider>,
    );

    expect(screen.getByTestId("label")).toHaveTextContent(
      dictionaries["en-US"]["nav.all"],
    );

    await user.click(screen.getByRole("button", { name: "switch language" }));

    expect(screen.getByTestId("label")).toHaveTextContent(
      dictionaries["zh-CN"]["nav.all"],
    );
  });
});

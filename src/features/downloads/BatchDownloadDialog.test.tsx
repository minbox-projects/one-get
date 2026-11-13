import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type { AddDownloadRequest } from "../../types/downloads";

const { addDownload } = vi.hoisted(() => ({ addDownload: vi.fn() }));

vi.mock("../../lib/downloads", () => ({ addDownload }));

import { BatchDownloadDialog, parseBatchUrls } from "./BatchDownloadDialog";

const en = dictionaries["en-US"];
const zh = dictionaries["zh-CN"];

const BATCH = [
  "https://good.test/a.bin",
  "ftp://bad.test/x",
  "   ",
  "https://good.test/a.bin",
  "not a url",
  "https://good.test/b.bin",
  "https://good.test/a.bin",
].join("\n");

function submitButton(container: HTMLElement): HTMLButtonElement {
  const buttons = within(container).getAllByRole("button");
  const match = buttons.find((button) => {
    const label = `${button.textContent ?? ""} ${button.getAttribute("aria-label") ?? ""}`;
    return (
      /^(start|add|create|begin|download)\b/i.test(label.trim()) &&
      !/header|cancel|close|batch|advanced/i.test(label)
    );
  });

  if (!match) {
    throw new Error("The batch submit button was not found.");
  }

  return match as HTMLButtonElement;
}

async function setField(
  user: ReturnType<typeof userEvent.setup>,
  field: HTMLElement,
  value: string,
) {
  if (field.tagName === "SELECT") {
    await user.selectOptions(field, value);
    return;
  }

  await user.clear(field);
  if (value !== "") {
    await user.type(field, value);
  }
}

async function fillFirstHeader(
  user: ReturnType<typeof userEvent.setup>,
  container: HTMLElement,
  name: string,
  value: string,
) {
  const toggles = within(container).queryAllByText(/advanced headers?/i);
  for (const toggle of toggles) {
    if (toggle.closest("button,summary") !== null) {
      await user.click(toggle);
      break;
    }
  }

  const addHeader = within(container).queryByRole("button", {
    name: /add header/i,
  });
  if (addHeader) {
    await user.click(addHeader);
  }

  const nameFields = within(container).getAllByLabelText(
    /header name|^name$/i,
  );
  const valueFields = within(container).getAllByLabelText(
    /header value|^value$/i,
  );
  const nameField = nameFields[nameFields.length - 1];
  const valueField = valueFields[valueFields.length - 1];

  await user.clear(nameField);
  await user.type(nameField, name);
  await user.clear(valueField);
  await user.type(valueField, value);
}

describe("parseBatchUrls", () => {
  it("reports invalid and duplicate lines while keeping the first occurrence", () => {
    expect(parseBatchUrls(BATCH)).toEqual({
      entries: [
        { line: 1, url: "https://good.test/a.bin" },
        { line: 6, url: "https://good.test/b.bin" },
      ],
      invalid: [
        { line: 2, code: "url_scheme" },
        { line: 5, code: "url_scheme" },
      ],
      duplicates: [
        { line: 4, url: "https://good.test/a.bin" },
        { line: 7, url: "https://good.test/a.bin" },
      ],
    });
  });

  it("returns no entries or errors for blank input", () => {
    expect(parseBatchUrls(" \n\t\n")).toEqual({
      entries: [],
      invalid: [],
      duplicates: [],
    });
  });
});

describe("BatchDownloadDialog", () => {
  beforeEach(() => {
    addDownload.mockReset();
    addDownload.mockResolvedValue({ ok: true, data: "task-batch" });
  });

  it("adds every valid unique URL with the shared options and reports line errors", async () => {
    const user = userEvent.setup();
    const onAdded = vi.fn();

    render(
      <I18nProvider language="en-US">
        <BatchDownloadDialog open onOpenChange={() => undefined} onAdded={onAdded} />
      </I18nProvider>,
    );

    const container = screen.getByRole("dialog", { name: /batch/i });
    await user.type(
      within(container).getByLabelText(/urls|one url per line/i),
      BATCH,
    );
    await setField(
      user,
      within(container).getByLabelText(/destination/i),
      "/tmp/batch",
    );
    await setField(
      user,
      within(container).getByLabelText(/connections/i),
      "6",
    );
    await setField(
      user,
      within(container).getByLabelText(/rate limit/i),
      "2048",
    );
    await fillFirstHeader(
      user,
      container,
      "Referer",
      "https://example.com/",
    );

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(2);
    });

    const requests = addDownload.mock.calls.map(
      ([request]) => request as AddDownloadRequest,
    );
    expect(requests.map((request) => request.url).sort()).toEqual([
      "https://good.test/a.bin",
      "https://good.test/b.bin",
    ]);
    for (const request of requests) {
      expect(request).toMatchObject({
        destination: "/tmp/batch",
        connections: 6,
        rateLimit: 2048,
      });
      expect(request.extraHeaders).toEqual([
        ["Referer", "https://example.com/"],
      ]);
    }

    expect(container).toHaveTextContent(/line\s*2/i);
    expect(container).toHaveTextContent(
      /(accepted|added)[^\d]*2|2[^\d]*(accepted|added)/i,
    );
    expect(container).toHaveTextContent(/(rejected|skipped|invalid)/i);
    await waitFor(() => {
      expect(onAdded).toHaveBeenCalled();
    });
  });

  it("rejects empty input without adding a task", async () => {
    const user = userEvent.setup();

    render(
      <I18nProvider language="en-US">
        <BatchDownloadDialog open onOpenChange={() => undefined} />
      </I18nProvider>,
    );

    const container = screen.getByRole("dialog", { name: /batch/i });
    const submit = submitButton(container);

    if (!submit.disabled) {
      await user.click(submit);
    }

    expect(addDownload).not.toHaveBeenCalled();
    expect(
      submit.disabled ||
        within(container).queryAllByRole("alert").length > 0 ||
        container.querySelectorAll('[aria-invalid="true"]').length > 0,
    ).toBe(true);
  });

  it("prefills captured URLs, shows the browser note, and adds one task per URL after submit", async () => {
    const user = userEvent.setup();
    const urls = [
      "https://good.test/captured-a.bin",
      "https://good.test/captured-b.bin",
    ];

    expect(en["batch.browserPrefill"]).toBeTypeOf("string");
    expect(zh["batch.browserPrefill"]).toBeTypeOf("string");

    render(
      <I18nProvider language="en-US">
        <BatchDownloadDialog
          open
          onOpenChange={() => undefined}
          prefillUrls={urls}
        />
      </I18nProvider>,
    );

    const container = screen.getByRole("dialog", { name: /batch/i });
    expect(
      within(container).getByLabelText(/urls|one url per line/i),
    ).toHaveValue(urls.join("\n"));
    expect(
      within(container).getByText(en["batch.browserPrefill"]),
    ).toBeInTheDocument();
    expect(addDownload).not.toHaveBeenCalled();

    await setField(
      user,
      within(container).getByLabelText(/destination/i),
      "/tmp/batch",
    );
    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(2);
    });

    const requests = addDownload.mock.calls.map(
      ([request]) => request as AddDownloadRequest,
    );
    expect(requests.map((request) => request.url).sort()).toEqual(urls);
    for (const request of requests) {
      expect(request.extraHeaders).toEqual([]);
    }
  });
});

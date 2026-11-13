import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { I18nProvider } from "../../i18n";
import type { AddDownloadRequest, ProbeResult } from "../../types/downloads";

const { addDownload, probeDownload } = vi.hoisted(() => ({
  addDownload: vi.fn(),
  probeDownload: vi.fn(),
}));

vi.mock("../../lib/downloads", () => ({ addDownload, probeDownload }));

import { NewDownloadDialog } from "./NewDownloadDialog";

const HEX_64 = "b".repeat(64);

function probe(overrides: Partial<ProbeResult> = {}): ProbeResult {
  return {
    finalUrl: "https://cdn.example.com/file.bin",
    totalBytes: 2048,
    rangeSupported: true,
    filename: "probed-file.bin",
    contentType: "application/octet-stream",
    etag: null,
    lastModified: null,
    ...overrides,
  };
}

function DialogHarness({
  onAdded = () => undefined,
}: {
  onAdded?: () => void;
}) {
  const [open, setOpen] = useState(true);

  return (
    <I18nProvider language="en-US">
      <NewDownloadDialog
        open={open}
        onOpenChange={setOpen}
        defaults={{ destination: "/tmp/downloads", connections: 8 }}
        onAdded={onAdded}
      />
    </I18nProvider>
  );
}

function dialog(): HTMLElement {
  return screen.getByRole("dialog", { name: /new download/i });
}

function submitButton(container: HTMLElement): HTMLButtonElement {
  const buttons = within(container).getAllByRole("button");
  const match = buttons.find((button) => {
    const label = `${button.textContent ?? ""} ${button.getAttribute("aria-label") ?? ""}`;
    return (
      /^(start|add|create|begin|download)\b/i.test(label.trim()) &&
      !/header|cancel|close|new download|advanced/i.test(label)
    );
  });

  if (!match) {
    throw new Error("The new-download submit button was not found.");
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

async function enterUrl(
  user: ReturnType<typeof userEvent.setup>,
  url: string,
) {
  await user.type(within(dialog()).getByLabelText(/url/i), url);
}

describe("NewDownloadDialog", () => {
  beforeEach(() => {
    addDownload.mockReset();
    probeDownload.mockReset();
    addDownload.mockResolvedValue({ ok: true, data: "task-1" });
    probeDownload.mockResolvedValue({ ok: true, data: probe() });
  });

  it("probes on URL change, fills the filename and size, and submits the reviewed values", async () => {
    const user = userEvent.setup();
    const onAdded = vi.fn();
    render(<DialogHarness onAdded={onAdded} />);

    const container = dialog();
    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalledWith({
        url: "https://example.com/file.bin",
        allowHtml: false,
        extraHeaders: [],
      });
    });

    const filename = within(container).getByLabelText(/filename/i);
    await waitFor(() => {
      expect(filename).toHaveValue("probed-file.bin");
    });
    expect(container).toHaveTextContent(/2\s?KB/i);
    expect(container).toHaveTextContent(/probed-file\.bin/i);

    await setField(
      user,
      within(container).getByLabelText(/connections/i),
      "4",
    );
    await setField(
      user,
      within(container).getByLabelText(/rate limit/i),
      "1024",
    );
    await setField(
      user,
      within(container).getByLabelText(/sha-?256/i),
      HEX_64,
    );
    await fillFirstHeader(
      user,
      container,
      "Referer",
      "https://example.com/",
    );

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });

    const request = addDownload.mock.calls[0][0] as AddDownloadRequest;
    expect(request).toMatchObject({
      url: "https://example.com/file.bin",
      filename: "probed-file.bin",
      destination: "/tmp/downloads",
      connections: 4,
      rateLimit: 1024,
      expectedSha256: HEX_64,
      allowHtml: false,
    });
    expect(request.extraHeaders).toEqual([
      ["Referer", "https://example.com/"],
    ]);

    await waitFor(() => {
      expect(onAdded).toHaveBeenCalledTimes(1);
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("dialog", { name: /new download/i }),
      ).not.toBeInTheDocument();
    });
  });

  it("blocks an HTML download until the explicit allowance is checked", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: true,
      data: probe({
        filename: "index.html",
        contentType: "text/html; charset=utf-8",
        totalBytes: 512,
      }),
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/index.html");

    const allowance = await within(container).findByRole("checkbox", {
      name: /html|allow|anyway/i,
    });

    await user.click(submitButton(container));
    expect(addDownload).not.toHaveBeenCalled();

    await user.click(allowance);
    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(addDownload.mock.calls[0][0]).toMatchObject({
      url: "https://example.com/index.html",
      filename: "index.html",
      allowHtml: true,
    });
  });

  it("states that one connection is used and pins the connection field when Range is unsupported", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: true,
      data: probe({ rangeSupported: false, filename: "single.bin" }),
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/single.bin");

    await waitFor(() => {
      expect(container).toHaveTextContent(/single connection|one connection/i);
    });

    const connections = within(container).getByLabelText(
      /connections/i,
    ) as HTMLInputElement;
    expect(connections.value).toBe("1");

    await user.click(submitButton(container));
    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(addDownload.mock.calls[0][0]).toMatchObject({
      filename: "single.bin",
      connections: 1,
    });
  });

  it("shows an actionable probe error and never adds the task", async () => {
    const user = userEvent.setup();
    probeDownload.mockResolvedValue({
      ok: false,
      error: {
        code: "http_error",
        message: "The server returned 404.",
      },
    });

    render(<DialogHarness />);
    const container = dialog();
    await enterUrl(user, "https://example.com/missing.bin");

    const alert = await within(container).findByRole("alert");
    expect(alert).toHaveTextContent(/\S+/);
    expect(container).not.toHaveTextContent("http_error");

    await setField(
      user,
      within(container).getByLabelText(/filename/i),
      "missing.bin",
    );
    await user.click(submitButton(container));

    expect(addDownload).not.toHaveBeenCalled();
    expect(within(container).getByRole("alert")).toBeInTheDocument();
  });

  it("shows localized validation errors and sends no request for invalid values", async () => {
    const user = userEvent.setup();
    render(<DialogHarness />);

    const container = dialog();
    await enterUrl(user, "https://example.com/file.bin");
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalled();
    });

    const filename = within(container).getByLabelText(/filename/i);
    await waitFor(() => {
      expect(filename).toHaveValue("probed-file.bin");
    });

    await setField(user, filename, "../evil.bin");
    await setField(
      user,
      within(container).getByLabelText(/connections/i),
      "65",
    );

    await user.click(submitButton(container));

    expect(addDownload).not.toHaveBeenCalled();
    expect(
      within(container).queryAllByRole("alert").length +
        container.querySelectorAll('[aria-invalid="true"]').length,
    ).toBeGreaterThan(0);
    expect(container).not.toHaveTextContent(
      /url_required|filename_traversal|connections_range|destination_required/,
    );
  });
});

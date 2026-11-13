import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { dictionaries, I18nProvider } from "../../i18n";
import type { ProbeResult } from "../../types/downloads";

const { addDownload, probeDownload, pickDirectory } = vi.hoisted(() => ({
  addDownload: vi.fn(),
  probeDownload: vi.fn(),
  pickDirectory: vi.fn(),
}));

vi.mock("../../lib/downloads", () => ({ addDownload, probeDownload }));
vi.mock("../../lib/dialog", () => ({ pickDirectory }));

import { NewDownloadDialog } from "./NewDownloadDialog";

const en = dictionaries["en-US"];

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

function DialogHarness() {
  const [open, setOpen] = useState(true);

  return (
    <I18nProvider language="en-US">
      <NewDownloadDialog
        open={open}
        onOpenChange={setOpen}
        defaults={{ destination: "/tmp/downloads", connections: 8 }}
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

beforeEach(() => {
  addDownload.mockReset();
  probeDownload.mockReset();
  pickDirectory.mockReset();
  addDownload.mockResolvedValue({ ok: true, data: "task-1" });
  probeDownload.mockResolvedValue({ ok: true, data: probe() });
});

describe("NewDownloadDialog destination handling", () => {
  it("calls the directory picker and fills the destination", async () => {
    const user = userEvent.setup();
    pickDirectory.mockResolvedValue({ ok: true, data: "/picked/folder" });

    render(<DialogHarness />);
    const container = dialog();
    const browse = within(container).getByRole("button", {
      name: /browse|choose|select.*(folder|directory)|directory/i,
    });

    await user.click(browse);

    expect(pickDirectory).toHaveBeenCalledTimes(1);
    await waitFor(() => {
      expect(within(container).getByLabelText(/destination/i)).toHaveValue(
        "/picked/folder",
      );
    });
  });

  it("shows the localized destination error instead of the generic add failure and keeps the dialog open", async () => {
    const user = userEvent.setup();
    expect(en["error.INVALID_DESTINATION"]).toBeTruthy();
    addDownload.mockResolvedValue({
      ok: false,
      error: {
        code: "INVALID_DESTINATION",
        message: "The destination is not a writable directory.",
      },
    });

    render(<DialogHarness />);
    const container = dialog();

    await user.type(
      within(container).getByLabelText(/url/i),
      "https://example.com/file.bin",
    );
    await waitFor(() => {
      expect(probeDownload).toHaveBeenCalled();
    });
    await waitFor(() => {
      expect(within(container).getByLabelText(/filename/i)).toHaveValue(
        "probed-file.bin",
      );
    });

    await user.click(submitButton(container));

    await waitFor(() => {
      expect(addDownload).toHaveBeenCalledTimes(1);
    });
    expect(
      await within(container).findByText(en["error.INVALID_DESTINATION"]),
    ).toBeInTheDocument();
    expect(
      within(container).queryByText(en["download.addFailed"]),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("dialog", { name: /new download/i }),
    ).toBeInTheDocument();
  });
});

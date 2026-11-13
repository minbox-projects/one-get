import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { I18nProvider } from "../../i18n";
import type { DownloadSummary } from "../../types/downloads";

import { DeleteConfirmDialog } from "./DeleteConfirmDialog";

function task(overrides: Partial<DownloadSummary> = {}): DownloadSummary {
  return {
    id: "task-1",
    url: "https://example.com/file.bin",
    filename: "file.bin",
    destination: "/tmp/downloads",
    state: "completed",
    downloadedBytes: 100,
    totalBytes: 100,
    connections: 8,
    createdAtMs: 1,
    updatedAtMs: 1,
    completedAtMs: 2,
    errorCode: null,
    errorMessage: null,
    ...overrides,
  };
}

function renderDialog(
  mode: "record" | "file",
  handlers: {
    onOpenChange?: (open: boolean) => void;
    onConfirm?: () => void;
  } = {},
) {
  return render(
    <I18nProvider language="en-US">
      <DeleteConfirmDialog
        open
        mode={mode}
        task={task()}
        onOpenChange={handlers.onOpenChange ?? vi.fn()}
        onConfirm={handlers.onConfirm ?? vi.fn()}
      />
    </I18nProvider>,
  );
}

function confirmButton(dialog: HTMLElement): HTMLElement {
  return within(dialog).getByRole("button", { name: /delete (record|file)/i });
}

describe("DeleteConfirmDialog", () => {
  it("confirms a record deletion while explaining that the final file is kept", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    renderDialog("record", { onConfirm });

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent(
      /file.*(keep|stays|remain|preserv)|(keep|preserve).*file/i,
    );

    const confirm = confirmButton(dialog);
    expect(confirm).toBeEnabled();
    await user.click(confirm);

    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("keeps cancel available without confirming record deletion", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onOpenChange = vi.fn();
    renderDialog("record", { onConfirm, onOpenChange });

    const dialog = screen.getByRole("dialog");
    await user.click(
      within(dialog).getAllByRole("button", { name: /cancel|close/i })[0],
    );

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it("requires an explicit acknowledgement before confirming a destructive file deletion", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    renderDialog("file", { onConfirm });

    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent(
      /cannot be undone|permanent|delete.*file.*(disk|computer)|remov.*file/i,
    );

    const confirm = confirmButton(dialog);
    expect(confirm).toBeDisabled();
    await user.click(confirm);
    expect(onConfirm).not.toHaveBeenCalled();

    await user.click(within(dialog).getByRole("checkbox"));
    expect(confirm).toBeEnabled();

    await user.click(confirm);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("keeps cancel available before the file-deletion acknowledgement", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    const onOpenChange = vi.fn();
    renderDialog("file", { onConfirm, onOpenChange });

    const dialog = screen.getByRole("dialog");
    await user.click(
      within(dialog).getAllByRole("button", { name: /cancel|close/i })[0],
    );

    expect(onOpenChange).toHaveBeenCalledWith(false);
    expect(onConfirm).not.toHaveBeenCalled();
  });
});

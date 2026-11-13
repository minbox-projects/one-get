import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { Toast } from "./Toast";

describe("Toast", () => {
  it("announces success and info messages as status", () => {
    const { unmount } = render(<Toast message="Download complete" tone="success" />);

    expect(screen.getByRole("status")).toHaveTextContent("Download complete");
    unmount();

    render(<Toast message="Download queued" tone="info" />);

    expect(screen.getByRole("status")).toHaveTextContent("Download queued");
  });

  it("announces errors as alerts", () => {
    render(<Toast message="Download failed" tone="error" />);

    expect(screen.getByRole("alert")).toHaveTextContent("Download failed");
  });

  it("exposes an accessible dismiss action", async () => {
    const user = userEvent.setup();
    const onDismiss = vi.fn();

    render(
      <Toast message="Download complete" tone="success" onDismiss={onDismiss} />,
    );

    await user.click(screen.getByRole("button", { name: /dismiss/i }));

    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});

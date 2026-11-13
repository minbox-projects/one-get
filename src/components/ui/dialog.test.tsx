import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { Dialog } from "./Dialog";

describe("Dialog", () => {
  it("opens from the trigger and exposes an accessible name and description", async () => {
    const user = userEvent.setup();

    render(
      <Dialog
        trigger={<button type="button">Open dialog</button>}
        title="Example dialog"
        description="Dialog description"
      >
        <p>Dialog body</p>
      </Dialog>,
    );

    await user.click(screen.getByRole("button", { name: "Open dialog" }));

    const dialog = screen.getByRole("dialog", { name: "Example dialog" });
    expect(dialog).toBeVisible();
    expect(screen.getByText("Dialog description")).toBeVisible();
  });

  it("closes on Escape and restores focus to the trigger", async () => {
    const user = userEvent.setup();

    render(
      <Dialog
        trigger={<button type="button">Open dialog</button>}
        title="Example dialog"
        description="Dialog description"
      >
        <p>Dialog body</p>
      </Dialog>,
    );

    const trigger = screen.getByRole("button", { name: "Open dialog" });
    await user.click(trigger);
    expect(screen.getByRole("dialog", { name: "Example dialog" })).toBeVisible();

    await user.keyboard("{Escape}");

    expect(
      screen.queryByRole("dialog", { name: "Example dialog" }),
    ).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });

  it("closes when the close control is activated", async () => {
    const user = userEvent.setup();

    render(
      <Dialog
        trigger={<button type="button">Open dialog</button>}
        title="Example dialog"
      >
        <p>Dialog body</p>
      </Dialog>,
    );

    await user.click(screen.getByRole("button", { name: "Open dialog" }));
    expect(screen.getByRole("dialog", { name: "Example dialog" })).toBeVisible();

    await user.click(screen.getByRole("button", { name: /close/i }));

    expect(
      screen.queryByRole("dialog", { name: "Example dialog" }),
    ).not.toBeInTheDocument();
  });
});

describe("Dialog controlled mode", () => {
  it("supports controlled open state without a trigger", async () => {
    const user = userEvent.setup();
    const onOpenChange = vi.fn();

    render(
      <Dialog
        open
        onOpenChange={onOpenChange}
        title="Controlled dialog"
        description="Controlled dialog description"
      >
        <p>Controlled body</p>
      </Dialog>,
    );

    expect(screen.getByRole("dialog", { name: "Controlled dialog" })).toBeVisible();

    await user.keyboard("{Escape}");

    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
});

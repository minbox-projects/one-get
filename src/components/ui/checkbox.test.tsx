import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { Checkbox } from "./Checkbox";

describe("Checkbox", () => {
  it("associates the label and toggles when the label is clicked", async () => {
    const user = userEvent.setup();
    render(<Checkbox label="Remember me" name="remember" />);

    const checkbox = screen.getByRole("checkbox", { name: "Remember me" });
    expect(checkbox).not.toBeChecked();

    await user.click(screen.getByText("Remember me"));

    expect(checkbox).toBeChecked();
  });

  it("exposes and associates error text with the checkbox", () => {
    render(
      <Checkbox label="Accept terms" name="terms" error="Accepting is required" />,
    );

    const checkbox = screen.getByRole("checkbox", { name: "Accept terms" });
    const error = screen.getByText("Accepting is required");

    expect(checkbox).toHaveAttribute("aria-invalid", "true");
    expect(checkbox.getAttribute("aria-describedby")).toContain(error.id);
  });
});

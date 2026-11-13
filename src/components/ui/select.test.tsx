import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { Select } from "./Select";

const OPTIONS = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

describe("Select", () => {
  it("associates the label with the native select", () => {
    render(<Select label="Theme" name="theme" options={OPTIONS} />);

    const select = screen.getByLabelText("Theme");
    expect(select.tagName).toBe("SELECT");
    expect(screen.getByRole("option", { name: "Light" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Dark" })).toBeInTheDocument();
  });

  it("updates its value when an option is selected", async () => {
    const user = userEvent.setup();
    render(
      <Select label="Theme" name="theme" options={OPTIONS} defaultValue="light" />,
    );

    await user.selectOptions(screen.getByLabelText("Theme"), "dark");

    expect(screen.getByLabelText("Theme")).toHaveValue("dark");
  });

  it("exposes and associates error text with the select", () => {
    render(
      <Select
        label="Theme"
        name="theme"
        options={OPTIONS}
        error="Theme is required"
      />,
    );

    const select = screen.getByLabelText("Theme");
    const error = screen.getByText("Theme is required");

    expect(select).toHaveAttribute("aria-invalid", "true");
    expect(select.getAttribute("aria-describedby")).toContain(error.id);
  });
});

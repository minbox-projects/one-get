import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { Input } from "./Input";

describe("Input", () => {
  it("associates the label with the input", () => {
    render(<Input label="Email" name="email" />);

    const input = screen.getByLabelText("Email");
    expect(input).toBeInTheDocument();
    expect(input.tagName).toBe("INPUT");
  });

  it("exposes and associates error text with the input", () => {
    render(<Input label="Email" name="email" error="Email is required" />);

    const input = screen.getByLabelText("Email");
    const error = screen.getByText("Email is required");

    expect(error).toBeVisible();
    expect(input).toHaveAttribute("aria-describedby");
    expect(input.getAttribute("aria-describedby")).toContain(error.id);
  });

  it("updates its value when typed into", async () => {
    const user = userEvent.setup();
    render(<Input label="Email" name="email" />);

    const input = screen.getByLabelText("Email");
    await user.type(input, "hello@example.com");

    expect(input).toHaveValue("hello@example.com");
  });

  it("prevents changes when disabled", async () => {
    const user = userEvent.setup();
    render(<Input label="Email" name="email" defaultValue="start" disabled />);

    const input = screen.getByLabelText("Email");
    await user.type(input, "more");

    expect(input).toHaveValue("start");
  });
});

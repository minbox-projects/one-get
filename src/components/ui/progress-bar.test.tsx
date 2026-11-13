import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { ProgressBar } from "./ProgressBar";

describe("ProgressBar", () => {
  it("exposes the known progress value and default range", () => {
    render(<ProgressBar value={40} label="Download progress" />);

    const bar = screen.getByRole("progressbar", {
      name: "Download progress",
    });

    expect(bar).toHaveAttribute("aria-valuemin", "0");
    expect(bar).toHaveAttribute("aria-valuemax", "100");
    expect(bar).toHaveAttribute("aria-valuenow", "40");
  });

  it("honors a custom maximum", () => {
    render(<ProgressBar value={50} max={250} label="Download progress" />);

    const bar = screen.getByRole("progressbar", {
      name: "Download progress",
    });

    expect(bar).toHaveAttribute("aria-valuemax", "250");
    expect(bar).toHaveAttribute("aria-valuenow", "50");
  });

  it("omits the value when progress is unknown", () => {
    render(<ProgressBar value={null} label="Download progress" indeterminate />);

    const bar = screen.getByRole("progressbar", {
      name: "Download progress",
    });

    expect(bar).toHaveAttribute("aria-valuemin", "0");
    expect(bar).toHaveAttribute("aria-valuemax", "100");
    expect(bar).not.toHaveAttribute("aria-valuenow");
  });
});

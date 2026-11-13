import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Badge } from "./Badge";

describe("Badge", () => {
  it("defaults to the neutral tone", () => {
    render(<Badge>Default</Badge>);

    expect(screen.getByText("Default")).toHaveClass("og-badge--neutral");
  });

  it("exposes a tone styling hook for every tone", () => {
    const tones = ["neutral", "info", "success", "warning", "danger"] as const;

    for (const tone of tones) {
      const { unmount } = render(<Badge tone={tone}>{tone}</Badge>);

      expect(screen.getByText(tone)).toHaveClass(`og-badge--${tone}`);
      unmount();
    }
  });
});

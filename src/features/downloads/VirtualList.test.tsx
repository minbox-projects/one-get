import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { VirtualList } from "./VirtualList";

const ITEM_HEIGHT = 40;
const VIEWPORT_HEIGHT = 400;
const ITEMS = Array.from({ length: 10_000 }, (_, index) => `Task ${index}`);

function renderList() {
  return render(
    <VirtualList
      items={ITEMS}
      itemHeight={ITEM_HEIGHT}
      height={VIEWPORT_HEIGHT}
      overscan={5}
      ariaLabel="Downloads"
      renderItem={(item) => <span>{item}</span>}
    />,
  );
}

function scrollTo(list: HTMLElement, index: number) {
  list.scrollTop = index * ITEM_HEIGHT;
  fireEvent.scroll(list);
}

describe("VirtualList", () => {
  it("renders only the visible window plus overscan for 10,000 items", () => {
    renderList();

    const rows = screen.getAllByTestId("task-row");
    expect(rows.length).toBeGreaterThan(0);
    expect(rows.length).toBeLessThanOrEqual(100);
    expect(screen.queryByText("Task 9999")).not.toBeInTheDocument();
  });

  it("exposes a labelled, keyboard-focusable list container", () => {
    renderList();

    const list = screen.getByRole("list", { name: "Downloads" });
    expect(list).toHaveAttribute("tabindex", "0");
  });

  it("scrolls to the first, middle, and last task and updates the mounted window", () => {
    renderList();

    const list = screen.getByRole("list", { name: "Downloads" });

    for (const index of [0, 5_000, 9_999]) {
      scrollTo(list, index);

      const row = screen.getByText(`Task ${index}`).closest(
        '[data-testid="task-row"]',
      );
      expect(row).not.toBeNull();
      expect(row).toHaveAttribute("data-index", String(index));
      expect(screen.getAllByTestId("task-row").length).toBeLessThanOrEqual(100);
    }

    expect(screen.queryByText("Task 0")).not.toBeInTheDocument();
    expect(screen.getByText("Task 9999")).toBeInTheDocument();
  });
});

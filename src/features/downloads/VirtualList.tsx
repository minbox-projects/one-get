import { useState } from "react";
import type { ReactNode, UIEvent } from "react";

export interface VirtualListProps<T> {
  items: readonly T[];
  itemHeight: number;
  height: number;
  overscan?: number;
  renderItem: (item: T, index: number) => ReactNode;
  ariaLabel: string;
  /** Identifies an item for selection state and row-level keyboard routing. */
  getItemKey?: (item: T) => string;
  isSelected?: (item: T) => boolean;
}

/**
 * Renders a fixed-height, windowed list so large task sets keep a bounded
 * number of mounted rows while remaining scrollable and keyboard focusable.
 */
export function VirtualList<T>({
  items,
  itemHeight,
  height,
  overscan = 5,
  renderItem,
  ariaLabel,
  getItemKey,
  isSelected,
}: VirtualListProps<T>) {
  const [scrollTop, setScrollTop] = useState(0);

  const visibleCount = Math.max(1, Math.ceil(height / itemHeight));
  const startIndex = Math.max(
    0,
    Math.floor(scrollTop / itemHeight) - overscan,
  );
  const endIndex = Math.min(
    items.length,
    startIndex + visibleCount + overscan * 2,
  );

  const handleScroll = (event: UIEvent<HTMLDivElement>) => {
    setScrollTop(event.currentTarget.scrollTop);
  };

  return (
    <div
      role="list"
      aria-label={ariaLabel}
      tabIndex={0}
      onScroll={handleScroll}
      data-testid="virtual-list"
      className="og-virtual-list"
      style={{ height, overflowY: "auto", position: "relative" }}
    >
      <div
        className="og-virtual-list__canvas"
        style={{ height: items.length * itemHeight, position: "relative" }}
      >
        {items.slice(startIndex, endIndex).map((item, offset) => {
          const index = startIndex + offset;
          const selected = isSelected?.(item) ?? false;
          return (
            <div
              key={getItemKey?.(item) ?? index}
              role="listitem"
              aria-selected={selected ? "true" : "false"}
              tabIndex={0}
              data-testid="task-row"
              data-index={index}
              data-task-id={getItemKey?.(item)}
              className="og-virtual-list__row"
              style={{
                position: "absolute",
                top: index * itemHeight,
                left: 0,
                right: 0,
                height: itemHeight,
              }}
            >
              {renderItem(item, index)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

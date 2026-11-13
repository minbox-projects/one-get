import type { ReactNode } from "react";

import type { DownloadSummary } from "../../types/downloads";

import { TaskRow } from "./TaskRow";
import { VirtualList } from "./VirtualList";

const ROW_HEIGHT = 84;
const DEFAULT_HEIGHT = 520;

export interface TaskListProps {
  tasks: readonly DownloadSummary[];
  ariaLabel: string;
  speedOf: (task: DownloadSummary) => number | null;
  renderActions?: (task: DownloadSummary) => ReactNode;
  height?: number;
}

/** Virtualized list of task rows over the complete filtered data set. */
export function TaskList({
  tasks,
  ariaLabel,
  speedOf,
  renderActions,
  height = DEFAULT_HEIGHT,
}: TaskListProps) {
  return (
    <VirtualList
      items={tasks}
      itemHeight={ROW_HEIGHT}
      height={height}
      ariaLabel={ariaLabel}
      renderItem={(task) => (
        <TaskRow
          task={task}
          speed={speedOf(task)}
          actions={renderActions?.(task)}
        />
      )}
    />
  );
}

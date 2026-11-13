import type { ReactNode } from "react";

import type { DownloadSummary } from "../../types/downloads";

import { TaskRow } from "./TaskRow";
import { VirtualList } from "./VirtualList";

const ROW_HEIGHT = 108;
const DEFAULT_HEIGHT = 520;

export interface TaskListProps {
  tasks: readonly DownloadSummary[];
  ariaLabel: string;
  speedOf: (task: DownloadSummary) => number | null;
  renderActions?: (task: DownloadSummary) => ReactNode;
  selectedId?: string | null;
  onSelect?: (task: DownloadSummary) => void;
  onPause?: (task: DownloadSummary) => void;
  onResume?: (task: DownloadSummary) => void;
  onCancel?: (task: DownloadSummary) => void;
  onRetry?: (task: DownloadSummary) => void;
  height?: number;
}

/** Virtualized list of task rows over the complete filtered data set. */
export function TaskList({
  tasks,
  ariaLabel,
  speedOf,
  renderActions,
  selectedId,
  onSelect,
  onPause,
  onResume,
  onCancel,
  onRetry,
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
          selected={selectedId === task.id}
          onSelect={onSelect}
          onPause={onPause}
          onResume={onResume}
          onCancel={onCancel}
          onRetry={onRetry}
        />
      )}
    />
  );
}

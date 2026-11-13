import type {
  DownloadCategory,
  DownloadState,
  DownloadSummary,
} from "../../types/downloads";

import { categoryForFilename } from "./categories";
import { domainFromUrl, progressPercent } from "./format";

export type SidebarView = "all" | "downloading" | "completed" | "failed" | DownloadCategory;

export type SortKey =
  | "createdAt"
  | "filename"
  | "size"
  | "progress"
  | "speed"
  | "state";

export type SortDirection = "asc" | "desc";

export const STATE_SORT_ORDER: readonly DownloadState[] = [
  "downloading",
  "connecting",
  "finalizing",
  "paused",
  "queued",
  "failed",
  "completed",
  "cancelled",
];

const ACTIVE_STATES: readonly DownloadState[] = [
  "downloading",
  "connecting",
  "finalizing",
];

const CATEGORY_VIEWS: readonly DownloadCategory[] = [
  "video",
  "model",
  "document",
  "program",
  "other",
];

function matchesView(task: DownloadSummary, view: SidebarView): boolean {
  if (view === "all") {
    return true;
  }
  if (view === "downloading") {
    return ACTIVE_STATES.includes(task.state);
  }
  if (view === "completed") {
    return task.state === "completed";
  }
  if (view === "failed") {
    return task.state === "failed";
  }
  return categoryForFilename(task.filename) === view;
}

function matchesSearch(task: DownloadSummary, search: string): boolean {
  const needle = search.trim().toLowerCase();
  if (needle.length === 0) {
    return true;
  }

  if (task.filename.toLowerCase().includes(needle)) {
    return true;
  }

  const domain = domainFromUrl(task.url);
  return domain !== null && domain.toLowerCase().includes(needle);
}

/** Filters the full task set by sidebar view and keyword search. */
export function filterTasks(
  tasks: readonly DownloadSummary[],
  view: SidebarView,
  search: string,
): DownloadSummary[] {
  return tasks.filter(
    (task) => matchesView(task, view) && matchesSearch(task, search),
  );
}

function compareValues(
  left: number | null,
  right: number | null,
): number {
  const leftValue = left ?? -1;
  const rightValue = right ?? -1;
  return leftValue - rightValue;
}

function compareTasks(
  left: DownloadSummary,
  right: DownloadSummary,
  key: SortKey,
  speedOf?: (task: DownloadSummary) => number | null,
): number {
  switch (key) {
    case "createdAt":
      return left.createdAtMs - right.createdAtMs;
    case "filename":
      return left.filename.localeCompare(right.filename);
    case "size":
      return compareValues(left.totalBytes, right.totalBytes);
    case "progress":
      return compareValues(progressPercent(left), progressPercent(right));
    case "speed":
      return compareValues(speedOf?.(left) ?? null, speedOf?.(right) ?? null);
    case "state":
      return (
        STATE_SORT_ORDER.indexOf(left.state) -
        STATE_SORT_ORDER.indexOf(right.state)
      );
  }
}

/** Sorts a copy of the full task set without mutating the input. */
export function sortTasks(
  tasks: readonly DownloadSummary[],
  key: SortKey,
  direction: SortDirection,
  speedOf?: (task: DownloadSummary) => number | null,
): DownloadSummary[] {
  return [...tasks].sort((left, right) => {
    if (key === "state") {
      const byState = compareTasks(left, right, key);
      if (byState !== 0) {
        return byState;
      }
      return right.createdAtMs - left.createdAtMs;
    }

    const result = compareTasks(left, right, key, speedOf);
    return direction === "asc" ? result : -result;
  });
}

const SIDEBAR_VIEWS: readonly SidebarView[] = [
  "all",
  "downloading",
  "completed",
  "failed",
  ...CATEGORY_VIEWS,
];

/** Counts every sidebar view against the full task set. */
export function sidebarCounts(
  tasks: readonly DownloadSummary[],
): Record<SidebarView, number> {
  const counts = {} as Record<SidebarView, number>;
  for (const view of SIDEBAR_VIEWS) {
    counts[view] = filterTasks(tasks, view, "").length;
  }
  return counts;
}

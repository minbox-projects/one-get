import { useMemo } from "react";

import { Button } from "../../components/ui/Button";
import { useI18n } from "../../i18n";
import type { AppError } from "../../types/app";
import type { DownloadSummary } from "../../types/downloads";

import type { SidebarView, SortDirection, SortKey } from "./filters";
import { filterTasks, sortTasks } from "./filters";
import { TaskList } from "./TaskList";

export interface DownloadsViewProps {
  tasks: readonly DownloadSummary[];
  loading: boolean;
  error: AppError | null;
  view: SidebarView;
  search: string;
  sortKey: SortKey;
  sortDirection: SortDirection;
  speedOf: (task: DownloadSummary) => number | null;
  ariaLabel: string;
  onRetry: () => void;
  selectedId?: string | null;
  onSelect?: (task: DownloadSummary) => void;
  onPause?: (task: DownloadSummary) => void;
  onResume?: (task: DownloadSummary) => void;
  onCancel?: (task: DownloadSummary) => void;
  onRetryTask?: (task: DownloadSummary) => void;
}

/**
 * Composes the task list with loading, empty, filtered-empty and error states.
 */
export function DownloadsView({
  tasks,
  loading,
  error,
  view,
  search,
  sortKey,
  sortDirection,
  speedOf,
  ariaLabel,
  onRetry,
  selectedId,
  onSelect,
  onPause,
  onResume,
  onCancel,
  onRetryTask,
}: DownloadsViewProps) {
  const { t } = useI18n();

  const visibleTasks = useMemo(
    () => sortTasks(filterTasks(tasks, view, search), sortKey, sortDirection, speedOf),
    [tasks, view, search, sortKey, sortDirection, speedOf],
  );

  if (error !== null && tasks.length === 0) {
    return (
      <div className="og-list-error" role="alert">
        <p className="og-list-error__message">{t("error.downloadsLoad")}</p>
        <Button variant="secondary" onClick={onRetry}>
          {t("common.retry")}
        </Button>
      </div>
    );
  }

  if (loading && tasks.length === 0) {
    return (
      <div className="og-list-loading" role="status">
        <span className="og-visually-hidden">{t("common.loading")}</span>
        {[0, 1, 2].map((index) => (
          <span key={index} className="og-skeleton-row" aria-hidden="true" />
        ))}
      </div>
    );
  }

  if (tasks.length === 0) {
    return (
      <div className="og-empty">
        <h2 className="og-empty__title">{t("list.emptyTitle")}</h2>
        <p className="og-empty__body">{t("list.emptyBody")}</p>
      </div>
    );
  }

  if (visibleTasks.length === 0) {
    return (
      <div className="og-empty">
        <h2 className="og-empty__title">{t("list.filteredEmptyTitle")}</h2>
        <p className="og-empty__body">{t("list.filteredEmptyBody")}</p>
      </div>
    );
  }

  return (
    <TaskList
      tasks={visibleTasks}
      ariaLabel={ariaLabel}
      speedOf={speedOf}
      selectedId={selectedId}
      onSelect={onSelect}
      onPause={onPause}
      onResume={onResume}
      onCancel={onCancel}
      onRetry={onRetryTask}
    />
  );
}

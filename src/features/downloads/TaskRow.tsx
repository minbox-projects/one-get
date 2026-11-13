import type { ReactNode } from "react";

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { ProgressBar } from "../../components/ui/ProgressBar";
import { useI18n } from "../../i18n";
import type { DownloadSummary } from "../../types/downloads";

import { categoryForFilename } from "./categories";
import { taskActions } from "./controls";
import { errorMessageKey } from "./errorMessages";
import { domainFromUrl, progressPercent } from "./format";

export interface TaskRowProps {
  task: DownloadSummary;
  speed: number | null;
  actions?: ReactNode;
  selected?: boolean;
  onSelect?: (task: DownloadSummary) => void;
  onPause?: (task: DownloadSummary) => void;
  onResume?: (task: DownloadSummary) => void;
  onCancel?: (task: DownloadSummary) => void;
  onRetry?: (task: DownloadSummary) => void;
}

function remainingSeconds(task: DownloadSummary, speed: number | null): number | null {
  if (speed === null || speed <= 0 || task.totalBytes === null) {
    return null;
  }
  const remaining = task.totalBytes - task.downloadedBytes;
  if (remaining <= 0) {
    return 0;
  }
  return remaining / speed;
}

/** One task row showing state, metadata, progress and state-aware controls. */
export function TaskRow({
  task,
  speed,
  actions,
  selected = false,
  onSelect,
  onPause,
  onResume,
  onCancel,
  onRetry,
}: TaskRowProps) {
  const { t, formatBytes, formatNumber, formatSpeed, formatEta } = useI18n();

  const category = categoryForFilename(task.filename);
  const domain = domainFromUrl(task.url);
  const percent = progressPercent(task);
  const size = formatBytes(task.totalBytes);
  const speedValue = formatSpeed(speed);
  const eta = formatEta(remainingSeconds(task, speed));
  const unknown = t("common.unknown");
  const available = taskActions(task);

  return (
    <div
      className="og-task-row"
      data-selected={selected ? "true" : undefined}
    >
      <div className="og-task-row__primary">
        <Badge tone={task.state === "failed" ? "danger" : "info"}>
          {t(`state.${task.state}`)}
        </Badge>
        <button
          type="button"
          className="og-task-row__filename"
          onClick={() => onSelect?.(task)}
        >
          {task.filename}
        </button>
        <span className="og-task-row__domain">{domain ?? unknown}</span>
        <span className="og-task-row__category">{t(`nav.${category}`)}</span>
        {task.errorCode ? (
          <span className="og-task-row__error" title={task.errorCode}>
            {task.errorCode}: {t(errorMessageKey(task.errorCode))}
          </span>
        ) : null}
      </div>

      <div className="og-task-row__meta">
        <span className="og-task-row__metric">
          <span className="og-task-row__metric-label">{t("task.size")}</span>{" "}
          <span className="og-task-row__metric-value">{size ?? unknown}</span>
        </span>
        <span className="og-task-row__metric">
          <span className="og-task-row__metric-label">{t("task.speed")}</span>{" "}
          <span className="og-task-row__metric-value">
            {speedValue ?? unknown}
          </span>
        </span>
        <span className="og-task-row__metric">
          <span className="og-task-row__metric-label">{t("task.eta")}</span>{" "}
          <span className="og-task-row__metric-value">{eta ?? unknown}</span>
        </span>
        <span className="og-task-row__metric">
          <span className="og-task-row__metric-label">
            {t("task.connections")}
          </span>{" "}
          <span className="og-task-row__metric-value">
            {formatNumber(task.connections) ?? unknown}{" "}
          </span>
        </span>
      </div>

      <div className="og-task-row__progress">
        <span className="og-task-row__percent">
          {percent === null ? unknown : `${Math.round(percent)}%`}
        </span>
        <ProgressBar
          value={percent}
          label={t("task.progress", { filename: task.filename })}
        />
      </div>

      <div className="og-task-row__actions">
        {actions}
        <Button
          variant="ghost"
          disabled={!available.pause}
          onClick={() => onPause?.(task)}
        >
          {t("task.pause")}
        </Button>
        <Button
          variant="ghost"
          disabled={!available.resume}
          onClick={() => onResume?.(task)}
        >
          {t("task.resume")}
        </Button>
        <Button
          variant="ghost"
          disabled={!available.cancel}
          onClick={() => onCancel?.(task)}
        >
          {t("task.cancel")}
        </Button>
        <Button
          variant="ghost"
          disabled={!available.retry}
          onClick={() => onRetry?.(task)}
        >
          {t("task.retry")}
        </Button>
      </div>
    </div>
  );
}

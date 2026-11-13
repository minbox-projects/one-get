import type { ReactNode } from "react";

import { Badge } from "../../components/ui/Badge";
import { ProgressBar } from "../../components/ui/ProgressBar";
import { useI18n } from "../../i18n";
import type { DownloadSummary } from "../../types/downloads";

import { categoryForFilename } from "./categories";
import { domainFromUrl, progressPercent } from "./format";

export interface TaskRowProps {
  task: DownloadSummary;
  speed: number | null;
  actions?: ReactNode;
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

/** One task row showing state, metadata, progress and an optional action slot. */
export function TaskRow({ task, speed, actions }: TaskRowProps) {
  const { t, formatBytes, formatNumber, formatSpeed, formatEta } = useI18n();

  const category = categoryForFilename(task.filename);
  const domain = domainFromUrl(task.url);
  const percent = progressPercent(task);
  const size = formatBytes(task.totalBytes);
  const speedValue = formatSpeed(speed);
  const eta = formatEta(remainingSeconds(task, speed));
  const unknown = t("common.unknown");

  return (
    <div className="og-task-row">
      <div className="og-task-row__primary">
        <Badge tone={task.state === "failed" ? "danger" : "info"}>
          {t(`state.${task.state}`)}
        </Badge>
        <span className="og-task-row__filename">{task.filename}</span>
        <span className="og-task-row__domain">{domain ?? unknown}</span>
        <span className="og-task-row__category">{t(`nav.${category}`)}</span>
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
            {formatNumber(task.connections) ?? unknown}
          </span>
        </span>
      </div>
      {" "}

      <div className="og-task-row__progress">
        <span className="og-task-row__percent">
          {percent === null ? unknown : `${Math.round(percent)}%`}
        </span>
        <ProgressBar
          value={percent}
          label={t("task.progress", { filename: task.filename })}
        />
      </div>

      {actions ? <div className="og-task-row__actions">{actions}</div> : null}
    </div>
  );
}

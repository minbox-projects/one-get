import type { ReactNode } from "react";

import { Badge } from "../../components/ui/Badge";
import { Button } from "../../components/ui/Button";
import { ProgressBar } from "../../components/ui/ProgressBar";
import { useI18n } from "../../i18n";
import { redactStoredUrl } from "../../lib/downloads";
import type { AppError } from "../../types/app";
import type { DownloadSegment, DownloadSummary } from "../../types/downloads";

import { taskActions } from "./controls";
import { errorMessageKey } from "./errorMessages";

export interface TaskDetailsProps {
  task: DownloadSummary;
  segments: readonly DownloadSegment[];
  segmentsLoading?: boolean;
  segmentsError?: AppError | null;
  speed: number | null;
  onOpenFile?: (task: DownloadSummary) => void;
  onOpenFolder?: (task: DownloadSummary) => void;
  onCopyUrl?: (task: DownloadSummary) => void;
  onPause?: (task: DownloadSummary) => void;
  onResume?: (task: DownloadSummary) => void;
  onCancel?: (task: DownloadSummary) => void;
  onRetry?: (task: DownloadSummary) => void;
  onDeleteRecord?: (task: DownloadSummary) => void;
  onDeleteFile?: (task: DownloadSummary) => void;
  onClose?: () => void;
}

function remainingSeconds(task: DownloadSummary, speed: number | null): number | null {
  if (speed === null || speed <= 0 || task.totalBytes === null) {
    return null;
  }
  const remaining = task.totalBytes - task.downloadedBytes;
  return remaining <= 0 ? 0 : remaining / speed;
}

interface DetailRowProps {
  label: string;
  children: ReactNode;
}

function DetailRow({ label, children }: DetailRowProps) {
  return (
    <div className="og-details__row">
      <dt className="og-details__label">{label}</dt>{" "}
      <dd className="og-details__value">{children}</dd>
    </div>
  );
}

/** Full metadata, segment progress, and open/copy actions for one task. */
export function TaskDetails({
  task,
  segments,
  segmentsLoading = false,
  segmentsError = null,
  speed,
  onOpenFile,
  onOpenFolder,
  onCopyUrl,
  onPause,
  onResume,
  onCancel,
  onRetry,
  onDeleteRecord,
  onDeleteFile,
  onClose,
}: TaskDetailsProps) {
  const { t, formatBytes, formatDate, formatEta, formatNumber, formatSpeed } =
    useI18n();

  const available = taskActions(task);
  const unknown = t("common.unknown");
  const storedUrl = redactStoredUrl(task.url);
  const finalUrl = task.finalUrl ? redactStoredUrl(task.finalUrl) : null;
  const eta = formatEta(remainingSeconds(task, speed));
  const completedAt =
    task.completedAtMs === null ? null : formatDate(task.completedAtMs);

  return (
    <div className="og-details__body">
      <div className="og-details__heading">
        <Badge tone={task.state === "failed" ? "danger" : "info"}>
          {t(`state.${task.state}`)}
        </Badge>
        <Button variant="ghost" onClick={onClose}>
          {t("details.close")}
        </Button>
      </div>

      <dl className="og-details__grid">
        <DetailRow label={t("details.url")}>
          <span className="og-details__url">{storedUrl}</span>
        </DetailRow>
        {finalUrl ? (
          <DetailRow label={t("details.finalUrl")}>
            <span className="og-details__url">{finalUrl}</span>
          </DetailRow>
        ) : null}
        <DetailRow label={t("details.directory")}>
          {task.destination ?? unknown}
        </DetailRow>
        <DetailRow label={t("details.state")}>{t(`state.${task.state}`)}</DetailRow>
        <DetailRow label={t("details.totalSize")}>
          {formatBytes(task.totalBytes) ?? unknown}
        </DetailRow>
        <DetailRow label={t("details.downloaded")}>
          {formatBytes(task.downloadedBytes) ?? unknown}
        </DetailRow>
        <DetailRow label={t("details.speed")}>
          {formatSpeed(speed) ?? unknown}
        </DetailRow>
        <DetailRow label={t("details.eta")}>{eta ?? unknown}</DetailRow>
        <DetailRow label={t("details.connections")}>
          {formatNumber(task.connections) ?? unknown}{" "}
        </DetailRow>
        <DetailRow label={t("details.createdAt")}>
          {formatDate(task.createdAtMs) ?? unknown}
        </DetailRow>
        <DetailRow label={t("details.completedAt")}>
          {completedAt ?? unknown}
        </DetailRow>
      </dl>

      {task.errorCode || task.errorMessage ? (
        <section className="og-details__error" aria-label={t("details.error")}>
          {task.errorCode ? (
            <p className="og-details__error-code">{task.errorCode}</p>
          ) : null}
          <p className="og-details__error-message">
            {t(errorMessageKey(task.errorCode ?? ""))}
          </p>
        </section>
      ) : null}

      <section className="og-details__segments" aria-label={t("details.segments")}>
        <h3 className="og-details__section-title">{t("details.segments")}</h3>
        {segmentsLoading ? (
          <p className="og-details__segments-status" role="status">
            {t("details.segmentsLoading")}
          </p>
        ) : segmentsError ? (
          <p className="og-details__segments-status" role="alert">
            {segmentsError.message}
          </p>
        ) : segments.length === 0 ? (
          <p className="og-details__segments-status">
            {t("details.segmentsEmpty")}
          </p>
        ) : (
          <ul className="og-details__segment-list">
            {segments.map((segment) => {
              const length = segment.end - segment.start;
              const percent =
                length > 0 ? (segment.downloadedBytes / length) * 100 : null;
              return (
                <li className="og-details__segment" key={segment.index}>
                  <span className="og-details__segment-index">
                    {t("details.segment", { index: segment.index })}
                  </span>
                  <span className="og-details__segment-bytes">
                    {formatNumber(segment.downloadedBytes) ?? unknown} /{" "}
                    {formatNumber(length) ?? unknown}
                  </span>
                  <ProgressBar
                    value={percent}
                    label={t("details.segmentProgress", {
                      index: segment.index,
                    })}
                  />
                  <Badge
                    tone={segment.state === "failed" ? "danger" : "neutral"}
                  >
                    {t(`segment.${segment.state}`)}
                  </Badge>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <div className="og-details__actions">
        <Button
          variant="secondary"
          disabled={!available.pause}
          onClick={() => onPause?.(task)}
        >
          {t("task.pause")}
        </Button>
        <Button
          variant="secondary"
          disabled={!available.resume}
          onClick={() => onResume?.(task)}
        >
          {t("task.resume")}
        </Button>
        <Button
          variant="secondary"
          disabled={!available.cancel}
          onClick={() => onCancel?.(task)}
        >
          {t("task.cancel")}
        </Button>
        <Button
          variant="secondary"
          disabled={!available.retry}
          onClick={() => onRetry?.(task)}
        >
          {t("task.retry")}
        </Button>
      </div>

      <div className="og-details__actions">
        <Button
          variant="ghost"
          disabled={!available.openFile}
          onClick={() => onOpenFile?.(task)}
        >
          {t("details.openFile")}
        </Button>
        <Button
          variant="ghost"
          disabled={!available.openFolder}
          onClick={() => onOpenFolder?.(task)}
        >
          {t("details.openFolder")}
        </Button>
        <Button
          variant="ghost"
          disabled={!storedUrl}
          onClick={() => onCopyUrl?.(task)}
        >
          {t("details.copyUrl")}
        </Button>
        {available.deleteRecord ? (
          <Button variant="danger" onClick={() => onDeleteRecord?.(task)}>
            {t("delete.recordConfirm")}
          </Button>
        ) : null}
        {available.deleteFile ? (
          <Button variant="danger" onClick={() => onDeleteFile?.(task)}>
            {t("delete.fileConfirm")}
          </Button>
        ) : null}
      </div>
    </div>
  );
}
